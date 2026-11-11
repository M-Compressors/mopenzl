//! Private wire format, bounded assembly and state machines.

use alloc::vec::Vec;
use fearless_simd::Level;

#[cfg(feature = "decompression")]
use super::MetadataEntry;
#[cfg(feature = "compression")]
use super::MetadataField;
use super::{FramedError as E, FramingConfig, ResourceInfo};
use crate::shared::xxh3;

#[cfg(feature = "decompression")]
pub(super) mod decode;
#[cfg(feature = "compression")]
pub(super) mod encode;

pub(super) const MAGIC: &[u8; 8] = b"MOZFRM\x01\0";
pub(super) const FOOTER: &[u8; 8] = b"MOZEND\x01\0";
pub(super) const FOOTER_LEN: usize = 32;
pub(super) const RECORD_HEADER: usize = 24;
pub(super) const ENTRY_LEN: usize = 24;

pub(super) fn reserve<T>(vec: &mut Vec<T>, additional: usize) -> Result<(), E> {
    vec.try_reserve(additional).map_err(|_| E::Allocation)
}

pub(super) const fn add(a: u64, b: u64) -> Result<u64, E> {
    match a.checked_add(b) {
        Some(sum) => Ok(sum),
        None => Err(E::Overflow),
    }
}

pub(super) fn host(value: u64) -> Result<usize, E> {
    usize::try_from(value).map_err(|_| E::Overflow)
}

pub(super) const fn limit(value: u64, max: u64, name: &'static str) -> Result<(), E> {
    if value > max {
        Err(E::Limit(name))
    } else {
        Ok(())
    }
}

pub(super) fn validate(config: FramingConfig) -> Result<(), E> {
    limit(
        config.max_resources as u64,
        u32::MAX as u64,
        "resource count",
    )?;
    limit(
        config.max_metadata_bytes as u64,
        u32::MAX as u64,
        "metadata size",
    )?;
    if config.max_container_bytes < (MAGIC.len() + 12 + FOOTER_LEN) as u64 {
        return Err(E::Invalid(
            "container limit is smaller than an empty container",
        ));
    }
    Ok(())
}

/// Reads a fixed-width little-endian field with checked slicing.
#[cfg(feature = "decompression")]
pub(super) fn number<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N], E> {
    bytes
        .get(at..at.checked_add(N).ok_or(E::Overflow)?)
        .ok_or(E::UnexpectedEndOfInput)?
        .try_into()
        .map_err(|_| E::UnexpectedEndOfInput)
}

#[cfg(feature = "decompression")]
pub(super) fn check_magic(bytes: &[u8]) -> Result<(), E> {
    if bytes.len() != 8 || &bytes[..6] != b"MOZFRM" || bytes[7] != 0 {
        return Err(E::Invalid("magic"));
    }
    if bytes[6] != 1 {
        return Err(E::UnsupportedVersion(bytes[6]));
    }
    Ok(())
}

fn field(code: [u8; 2], value: &[u8], seen: &mut u8) -> Result<(), E> {
    let bit = match &code {
        b"id" if core::str::from_utf8(value).is_ok() => 1,
        b"mt" if value.len() == 8 => 2,
        _ if code.iter().all(u8::is_ascii_uppercase) => 0,
        _ => return Err(E::Invalid("metadata field")),
    };
    if *seen & bit != 0 {
        return Err(E::Invalid("duplicate reserved metadata field"));
    }
    *seen |= bit;
    Ok(())
}

#[cfg(feature = "compression")]
pub(super) fn metadata_into(
    fields: &[MetadataField<'_>],
    max: usize,
    dst: &mut Vec<u8>,
) -> Result<(), E> {
    let mut size = 0u64;
    let mut seen = 0;
    for item in fields {
        field(item.code, item.value, &mut seen)?;
        size = add(size, add(6, item.value.len() as u64)?)?;
        limit(size, max as u64, "metadata size")?;
    }
    reserve(dst, host(size)?)?;
    for item in fields {
        dst.extend_from_slice(&item.code);
        dst.extend_from_slice(&(item.value.len() as u32).to_le_bytes());
        dst.extend_from_slice(item.value);
    }
    Ok(())
}

#[cfg(feature = "decompression")]
pub(super) fn metadata(bytes: &[u8]) -> Result<Vec<MetadataEntry>, E> {
    let mut at = 0;
    let mut seen = 0;
    let mut fields = Vec::new();
    while at < bytes.len() {
        let code = number::<2>(bytes, at)?;
        let len = u32::from_le_bytes(number(bytes, at + 2)?) as usize;
        at += 6;
        let end = host(add(at as u64, len as u64)?)?;
        let value = bytes.get(at..end).ok_or(E::Invalid("metadata length"))?;
        field(code, value, &mut seen)?;
        let mut owned = Vec::new();
        reserve(&mut owned, value.len())?;
        owned.extend_from_slice(value);
        reserve(&mut fields, 1)?;
        fields.push(MetadataEntry { code, value: owned });
        at = end;
    }
    Ok(fields)
}

pub(super) fn index_size(count: usize) -> Result<usize, E> {
    count
        .checked_mul(ENTRY_LEN)
        .and_then(|n| n.checked_add(12))
        .ok_or(E::Overflow)
}

#[cfg(feature = "compression")]
pub(super) fn suffix(
    entries: &[ResourceInfo],
    offset: u64,
    simd: Level,
    config: FramingConfig,
) -> Result<Vec<u8>, E> {
    let len = index_size(entries.len())?;
    let total = add(add(offset, len as u64)?, FOOTER_LEN as u64)?;
    limit(total, config.max_container_bytes, "container size")?;
    let mut out = Vec::new();
    reserve(&mut out, len.checked_add(FOOTER_LEN).ok_or(E::Overflow)?)?;
    out.extend_from_slice(b"INDX");
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        out.extend_from_slice(&entry.offset.to_le_bytes());
        out.extend_from_slice(&entry.length.to_le_bytes());
        out.extend_from_slice(&entry.decoded_size.to_le_bytes());
    }
    out.extend_from_slice(&xxh3::hash32(simd, &out).to_le_bytes());
    out.extend_from_slice(FOOTER);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&(len as u64).to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    Ok(out)
}

#[cfg(feature = "decompression")]
pub(super) fn index(
    bytes: &[u8],
    offset: u64,
    simd: Level,
    config: FramingConfig,
) -> Result<Vec<ResourceInfo>, E> {
    if bytes.get(..4) != Some(b"INDX") {
        return Err(E::Invalid("index magic"));
    }
    let count = u32::from_le_bytes(number(bytes, 4)?) as usize;
    limit(count as u64, config.max_resources as u64, "resource count")?;
    let len = index_size(count)?;
    if bytes.len() != len {
        return Err(E::Invalid("index length"));
    }
    if xxh3::hash32(simd, &bytes[..len - 4]) != u32::from_le_bytes(number(bytes, len - 4)?) {
        return Err(E::Invalid("index checksum"));
    }
    let mut entries = Vec::new();
    reserve(&mut entries, count)?;
    let mut next = 8;
    let mut total = 0;
    for n in 0..count {
        let at = 8 + n * ENTRY_LEN;
        let entry = ResourceInfo {
            offset: u64::from_le_bytes(number(bytes, at)?),
            length: u64::from_le_bytes(number(bytes, at + 8)?),
            decoded_size: u64::from_le_bytes(number(bytes, at + 16)?),
        };
        if entry.offset != next || entry.length < (RECORD_HEADER + 4) as u64 {
            return Err(E::Invalid("resource directory range"));
        }
        limit(
            entry.length,
            add(
                add(RECORD_HEADER as u64 + 4, config.max_metadata_bytes as u64)?,
                config.max_frame_bytes as u64,
            )?,
            "resource size",
        )?;
        next = add(next, entry.length)?;
        total = add(total, entry.decoded_size)?;
        limit(total, config.max_output_bytes, "output size")?;
        entries.push(entry);
    }
    if next != offset {
        return Err(E::Invalid("directory does not cover resources"));
    }
    Ok(entries)
}

#[cfg(feature = "decompression")]
pub(super) fn footer(bytes: &[u8], offset: u64, len: u64, total: u64) -> Result<(), E> {
    if bytes.len() != FOOTER_LEN
        || bytes.get(..8) != Some(FOOTER)
        || u64::from_le_bytes(number(bytes, 8)?) != offset
        || u64::from_le_bytes(number(bytes, 16)?) != len
        || u64::from_le_bytes(number(bytes, 24)?) != total
    {
        return Err(E::Invalid("footer"));
    }
    Ok(())
}

#[cfg(all(test, feature = "compression", feature = "decompression"))]
mod tests;
