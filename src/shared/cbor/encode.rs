//! The CBOR encoder (`A1C_Encoder_*`, `A1C_Item_encode`,
//! `A1C_Item_encodedSize` in `shared/a1cbor.c`).
//!
//! Output is definite-length with the shortest argument encoding. Writes are
//! issued in the reference's granularity (a header byte, then its argument,
//! then the payload), because a full destination reports the number of bytes
//! written before the failing write.

use alloc::vec::Vec;

use super::item::{Error, ErrorKind, Item};

/// Where encoded bytes go (`A1C_Encoder_WriteCallback`): returns how many of
/// `data` were accepted; fewer than `data.len()` fails the encode.
trait Sink {
    fn write(&mut self, data: &[u8]) -> usize;
}

#[cfg(test)]
/// `A1C_noopWrite`: counts only.
struct Count;

#[cfg(test)]
impl Sink for Count {
    fn write(&mut self, data: &[u8]) -> usize {
        data.len()
    }
}

impl Sink for Vec<u8> {
    fn write(&mut self, data: &[u8]) -> usize {
        self.extend_from_slice(data);
        data.len()
    }
}

#[cfg(test)]
/// `A1C_bufferWrite`: fills a fixed buffer and truncates the last write.
struct Slice<'d> {
    dst: &'d mut [u8],
    used: usize,
}

#[cfg(test)]
impl Sink for Slice<'_> {
    fn write(&mut self, data: &[u8]) -> usize {
        let take = data.len().min(self.dst.len() - self.used);
        self.dst[self.used..self.used + take].copy_from_slice(&data[..take]);
        self.used += take;
        take
    }
}

struct Encoder<'s, S> {
    sink: &'s mut S,
    written: usize,
    depth: usize,
}

type Result<T> = core::result::Result<T, Error>;

impl<S: Sink> Encoder<'_, S> {
    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            kind,
            position: self.written,
            depth: self.depth,
        }
    }

    /// `A1C_Encoder_write`.
    fn write(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let written = self.sink.write(data);
        self.written += written;
        if written < data.len() {
            return Err(self.error(ErrorKind::WriteFailed));
        }
        Ok(())
    }

    /// `A1C_Encoder_encodeHeaderAndCount`.
    fn header_and_count(&mut self, major: u8, count: u64) -> Result<()> {
        let short: u8 = match count {
            0..=23 => count as u8,
            24..=0xFF => 24,
            0x100..=0xFFFF => 25,
            0x1_0000..=0xFFFF_FFFF => 26,
            _ => 27,
        };
        self.write(&[(major << 5) | short])?;
        match short {
            24 => self.write(&[count as u8]),
            25 => self.write(&(count as u16).to_be_bytes()),
            26 => self.write(&(count as u32).to_be_bytes()),
            27 => self.write(&count.to_be_bytes()),
            _ => Ok(()),
        }
    }

    fn encode_data(&mut self, major: u8, data: &[u8]) -> Result<()> {
        self.header_and_count(major, data.len() as u64)?;
        self.write(data)
    }

    /// `A1C_Encoder_encodeSpecial`.
    fn special(&mut self, item: &Item<'_>) -> Result<()> {
        match *item {
            Item::Bool(value) => self.header_and_count(7, if value { 21 } else { 20 }),
            Item::Null => self.header_and_count(7, 22),
            Item::Undefined => self.header_and_count(7, 23),
            Item::Simple(value) => {
                if (20..32).contains(&value) {
                    return Err(self.error(ErrorKind::InvalidSimpleValue));
                }
                self.header_and_count(7, u64::from(value))
            }
            Item::Float16(bits) => {
                self.write(&[(7 << 5) | 25])?;
                self.write(&bits.to_be_bytes())
            }
            Item::Float32(value) => {
                self.write(&[(7 << 5) | 26])?;
                self.write(&value.to_bits().to_be_bytes())
            }
            Item::Float64(value) => {
                self.write(&[(7 << 5) | 27])?;
                self.write(&value.to_bits().to_be_bytes())
            }
            _ => unreachable!("only simple items reach `special`"),
        }
    }

    /// `A1C_Encoder_encodeOne`.
    fn encode_one(&mut self, item: &Item<'_>) -> Result<()> {
        self.depth += 1;
        match item {
            Item::Int(value) => {
                if *value >= 0 {
                    self.header_and_count(0, *value as u64)?;
                } else {
                    self.header_and_count(1, !*value as u64)?;
                }
            }
            Item::Bytes(bytes) => self.encode_data(2, bytes)?,
            Item::Text(text) => self.encode_data(3, text)?,
            Item::Array(items) => {
                self.header_and_count(4, items.len() as u64)?;
                for child in items {
                    self.encode_one(child)?;
                }
            }
            Item::Map(pairs) => {
                self.header_and_count(5, pairs.len() as u64)?;
                for (key, value) in pairs {
                    self.encode_one(key)?;
                    self.encode_one(value)?;
                }
            }
            Item::Tag(tag, inner) => {
                self.header_and_count(6, *tag)?;
                self.encode_one(inner)?;
            }
            Item::Bool(_)
            | Item::Null
            | Item::Undefined
            | Item::Float16(_)
            | Item::Float32(_)
            | Item::Float64(_)
            | Item::Simple(_) => self.special(item)?,
        }
        self.depth -= 1;
        Ok(())
    }
}

/// Runs the encoder, returning the bytes written.
fn run<S: Sink>(item: &Item<'_>, sink: &mut S) -> Result<usize> {
    let mut encoder = Encoder {
        sink,
        written: 0,
        depth: 0,
    };
    encoder.encode_one(item)?;
    Ok(encoder.written)
}

#[cfg(test)]
/// The exact encoded size of `item` (`A1C_Item_encodedSize`).
///
/// # Errors
///
/// A simple value in the reserved range 20 to 31
/// ([`ErrorKind::InvalidSimpleValue`]).
pub(crate) fn encoded_size(item: &Item<'_>) -> Result<usize> {
    run(item, &mut Count)
}

/// Encodes `item` into a new vector.
///
/// # Errors
///
/// As [`encoded_size`].
pub(crate) fn encode_to_vec(item: &Item<'_>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    run(item, &mut out)?;
    Ok(out)
}

#[cfg(test)]
/// Encodes `item` into `dst` and returns the number of bytes written
/// (`A1C_Item_encode`).
///
/// # Errors
///
/// [`ErrorKind::WriteFailed`] (with the bytes written so far as the
/// position) when `dst` is too small, and as [`encoded_size`].
pub(crate) fn encode_to_slice(item: &Item<'_>, dst: &mut [u8]) -> Result<usize> {
    run(item, &mut Slice { dst, used: 0 })
}
