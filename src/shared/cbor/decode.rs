//! The CBOR decoder (`A1C_Decoder_*` in `shared/a1cbor.c`).
//!
//! Every check happens in the reference's order so that the failure kind,
//! the cursor position and the depth agree with it, including the corner
//! cases: the depth counter is not unwound on failure, the allocation limit
//! is charged at the same points as the reference's arena, and
//! `rejectUnknownSimple` rejects 24 before reading its payload byte.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::vec::Vec;

use super::item::{Error, ErrorKind, Item};

/// `A1C_MAX_DEPTH_DEFAULT`.
const MAX_DEPTH_DEFAULT: usize = 128;
/// `sizeof(A1C_Item)` on 64-bit hosts: what the reference's arena is charged
/// for each item.
const ITEM_BYTES: usize = 32;
/// `sizeof(A1C_Pair)`: a key and a value item.
const PAIR_BYTES: usize = 2 * ITEM_BYTES;

/// `A1C_DecoderConfig`. The default is the all-zero configuration of the
/// reference: default depth, no allocation limit, strings copied, unknown
/// simple values accepted.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct DecoderConfig {
    /// Maximum nesting depth; 0 means 128.
    pub(crate) max_depth: usize,
    /// Limit of the bytes the reference would allocate in one decode;
    /// 0 means unlimited.
    pub(crate) limit_bytes: usize,
    /// Borrow byte and text strings from the source instead of copying them.
    pub(crate) reference_source: bool,
    /// Reject simple values other than `false`, `true`, `null` and
    /// `undefined`.
    pub(crate) reject_unknown_simple: bool,
}

impl DecoderConfig {
    /// The configuration both OpenZL configuration loaders use: default
    /// depth, no limit, strings borrowed, unknown simple values rejected.
    pub(crate) const STRICT: Self = Self {
        max_depth: 0,
        limit_bytes: 0,
        reference_source: true,
        reject_unknown_simple: true,
    };
}

/// Major types (`A1C_MajorType`).
const UINT: u8 = 0;
const NEG: u8 = 1;
const BYTES: u8 = 2;
const TEXT: u8 = 3;
const ARRAY: u8 = 4;
const MAP: u8 = 5;
const TAG: u8 = 6;

fn major(header: u8) -> u8 {
    header >> 5
}

fn short_count(header: u8) -> u8 {
    header & 0x1F
}

fn is_break(header: u8) -> bool {
    header == 0xFF
}

fn is_indefinite(header: u8) -> bool {
    short_count(header) == 31
}

/// `A1C_ItemHeader_isLegal`.
fn is_legal(header: u8) -> bool {
    let count = short_count(header);
    if count >= 28 {
        if count < 31 {
            return false;
        }
        return !matches!(major(header), UINT | NEG | TAG);
    }
    true
}

/// Decodes the single item in `data`.
///
/// # Errors
///
/// The [`Error`] the reference's `A1C_Decoder_decode` reports: truncated
/// input, an illegal header, an integer outside `i64`, a nesting deeper than
/// `config.max_depth`, a stray break, a rejected simple value, trailing bytes,
/// or the allocation limit.
pub(crate) fn decode<'a>(
    data: &'a [u8],
    config: &DecoderConfig,
) -> core::result::Result<Item<'a>, Error> {
    let mut decoder = Decoder {
        data,
        pos: 0,
        depth: 0,
        max_depth: if config.max_depth == 0 {
            MAX_DEPTH_DEFAULT
        } else {
            config.max_depth
        },
        limit: config.limit_bytes,
        allocated: 0,
        reference_source: config.reference_source,
        reject_unknown_simple: config.reject_unknown_simple,
    };
    let item = decoder.decode_one()?;
    if decoder.pos < data.len() {
        return Err(decoder.error(ErrorKind::TrailingData));
    }
    Ok(item)
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    depth: usize,
    max_depth: usize,
    limit: usize,
    allocated: usize,
    reference_source: bool,
    reject_unknown_simple: bool,
}

type Result<T> = core::result::Result<T, Error>;

impl<'a> Decoder<'a> {
    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            kind,
            position: self.pos,
            depth: self.depth,
        }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// `A1C_Arena_calloc` through the limited arena: charges `count * size`
    /// bytes, nothing for an empty request.
    fn charge(&mut self, count: usize, size: usize) -> Result<()> {
        let total = count
            .checked_mul(size)
            .filter(|&bytes| bytes != 0)
            .map(|bytes| self.allocated.checked_add(bytes));
        match total {
            // An empty request allocates nothing.
            None => Ok(()),
            Some(Some(total)) if self.limit == 0 || total <= self.limit => {
                self.allocated = total;
                Ok(())
            }
            // Over the limit, or a size that overflows.
            Some(_) => Err(self.error(ErrorKind::BadAlloc)),
        }
    }

    fn peek_byte(&self) -> Result<u8> {
        self.data
            .get(self.pos)
            .copied()
            .ok_or_else(|| self.error(ErrorKind::Truncated))
    }

    fn skip(&mut self, size: usize) -> Result<()> {
        if self.remaining() < size {
            return Err(self.error(ErrorKind::Truncated));
        }
        self.pos += size;
        Ok(())
    }

    fn read_byte(&mut self) -> Result<u8> {
        let byte = self.peek_byte()?;
        self.pos += 1;
        Ok(byte)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let bytes = self
            .data
            .get(self.pos..self.pos + N)
            .ok_or_else(|| self.error(ErrorKind::Truncated))?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        self.pos += N;
        Ok(out)
    }

    /// `A1C_Decoder_readCount`.
    fn read_count(&mut self, header: u8) -> Result<u64> {
        let count = short_count(header);
        Ok(match count {
            0..=23 | 31 => u64::from(count),
            24 => u64::from(self.read_byte()?),
            25 => u64::from(u16::from_be_bytes(self.read_array()?)),
            26 => u64::from(u32::from_be_bytes(self.read_array()?)),
            _ => u64::from_be_bytes(self.read_array()?),
        })
    }

    /// `A1C_Decoder_readSize`.
    fn read_size(&mut self, header: u8) -> Result<usize> {
        let count = self.read_count(header)?;
        match usize::try_from(count) {
            Ok(size) => Ok(size),
            Err(_) => Err(self.error(ErrorKind::IntegerOverflow)),
        }
    }

    /// `A1C_Decoder_decodeOne`: a fresh item.
    fn decode_one(&mut self) -> Result<Item<'a>> {
        self.charge(1, ITEM_BYTES)?;
        self.decode_one_into()
    }

    /// `A1C_Decoder_decodeOneInto`.
    fn decode_one_into(&mut self) -> Result<Item<'a>> {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(self.error(ErrorKind::MaxDepthExceeded));
        }
        let header = self.read_byte()?;
        if !is_legal(header) {
            return Err(self.error(ErrorKind::InvalidItemHeader));
        }
        let item = match major(header) {
            UINT => {
                let value = self.read_count(header)?;
                match i64::try_from(value) {
                    Ok(value) => Item::Int(value),
                    Err(_) => return Err(self.error(ErrorKind::LargeIntegersUnsupported)),
                }
            }
            NEG => {
                let value = self.read_count(header)?;
                if value >= 1 << 63 {
                    return Err(self.error(ErrorKind::LargeIntegersUnsupported));
                }
                Item::Int(!(value as i64))
            }
            BYTES | TEXT => self.decode_data(header)?,
            ARRAY => self.decode_array(header)?,
            MAP => self.decode_map(header)?,
            TAG => self.decode_tag(header)?,
            // Major type 7.
            _ => self.decode_special(header)?,
        };
        self.depth -= 1;
        Ok(item)
    }

    fn string_item(header: u8, data: Cow<'a, [u8]>) -> Item<'a> {
        if major(header) == BYTES {
            Item::Bytes(data)
        } else {
            Item::Text(data)
        }
    }

    /// `A1C_Decoder_decodeDataDefinite`.
    fn decode_data_definite(&mut self, header: u8, reference_source: bool) -> Result<Item<'a>> {
        let size = self.read_size(header)?;
        if self.remaining() < size {
            return Err(self.error(ErrorKind::Truncated));
        }
        let source: &'a [u8] = self.data;
        let data = &source[self.pos..self.pos + size];
        let data = if reference_source {
            self.skip(size)?;
            Cow::Borrowed(data)
        } else {
            self.charge(size, 1)?;
            self.skip(size)?;
            Cow::Owned(data.to_vec())
        };
        Ok(Self::string_item(header, data))
    }

    /// `A1C_Decoder_decodeData`.
    fn decode_data(&mut self, header: u8) -> Result<Item<'a>> {
        if !is_indefinite(header) {
            return self.decode_data_definite(header, self.reference_source);
        }
        let mut total = 0usize;
        let mut chunks: Vec<&'a [u8]> = Vec::new();
        loop {
            let child = self.read_byte()?;
            if !is_legal(child) {
                return Err(self.error(ErrorKind::InvalidItemHeader));
            }
            if is_break(child) {
                break;
            }
            if major(child) != major(header) || is_indefinite(child) {
                return Err(self.error(ErrorKind::InvalidChunkedString));
            }
            self.charge(1, ITEM_BYTES)?;
            let chunk = match self.decode_data_definite(child, true)? {
                Item::Bytes(Cow::Borrowed(chunk)) | Item::Text(Cow::Borrowed(chunk)) => chunk,
                _ => unreachable!("a referenced chunk borrows the source"),
            };
            total = match total.checked_add(chunk.len()) {
                Some(total) => total,
                None => return Err(self.error(ErrorKind::IntegerOverflow)),
            };
            chunks.push(chunk);
        }
        self.charge(total, 1)?;
        let mut data = Vec::with_capacity(total);
        for chunk in chunks {
            data.extend_from_slice(chunk);
        }
        Ok(Self::string_item(header, Cow::Owned(data)))
    }

    /// `A1C_Decoder_decodeArray`.
    fn decode_array(&mut self, header: u8) -> Result<Item<'a>> {
        let size = self.read_size(header)?;
        let mut items = Vec::new();
        if is_indefinite(header) {
            loop {
                if is_break(self.peek_byte()?) {
                    self.skip(1)?;
                    break;
                }
                items.push(self.decode_one()?);
            }
            self.charge(items.len(), ITEM_BYTES)?;
        } else {
            if self.remaining() < size {
                // Each item takes at least one byte; checked before allocating.
                return Err(self.error(ErrorKind::Truncated));
            }
            self.charge(size, ITEM_BYTES)?;
            items.reserve_exact(size);
            for _ in 0..size {
                items.push(self.decode_one_into()?);
            }
        }
        Ok(Item::Array(items))
    }

    /// `A1C_Decoder_decodeMap`.
    fn decode_map(&mut self, header: u8) -> Result<Item<'a>> {
        let size = self.read_size(header)?;
        let mut pairs = Vec::new();
        if is_indefinite(header) {
            loop {
                if is_break(self.peek_byte()?) {
                    self.skip(1)?;
                    break;
                }
                let key = self.decode_one()?;
                let value = self.decode_one()?;
                pairs.push((key, value));
            }
            self.charge(pairs.len(), PAIR_BYTES)?;
        } else {
            if self.remaining() < size {
                return Err(self.error(ErrorKind::Truncated));
            }
            self.charge(size, PAIR_BYTES)?;
            pairs.reserve_exact(size);
            for _ in 0..size {
                let key = self.decode_one_into()?;
                let value = self.decode_one_into()?;
                pairs.push((key, value));
            }
        }
        Ok(Item::Map(pairs))
    }

    /// `A1C_Decoder_decodeTag`.
    fn decode_tag(&mut self, header: u8) -> Result<Item<'a>> {
        let tag = self.read_count(header)?;
        self.charge(1, ITEM_BYTES)?;
        let inner = self.decode_one_into()?;
        Ok(Item::Tag(tag, Box::new(inner)))
    }

    /// `A1C_Decoder_decodeSpecial`.
    fn decode_special(&mut self, header: u8) -> Result<Item<'a>> {
        let count = short_count(header);
        Ok(match count {
            20 | 21 => Item::Bool(count == 21),
            22 => Item::Null,
            23 => Item::Undefined,
            24 => {
                if self.reject_unknown_simple {
                    return Err(self.error(ErrorKind::InvalidSimpleEncoding));
                }
                let value = self.read_byte()?;
                if value < 32 {
                    return Err(self.error(ErrorKind::InvalidSimpleEncoding));
                }
                Item::Simple(value)
            }
            25 => Item::Float16(u16::from_be_bytes(self.read_array()?)),
            26 => Item::Float32(f32::from_bits(u32::from_be_bytes(self.read_array()?))),
            27 => Item::Float64(f64::from_bits(u64::from_be_bytes(self.read_array()?))),
            0..=19 => {
                if self.reject_unknown_simple {
                    return Err(self.error(ErrorKind::InvalidSimpleEncoding));
                }
                Item::Simple(count)
            }
            // 28 to 30 never pass `is_legal`; only the break code is left.
            _ => return Err(self.error(ErrorKind::BreakNotAllowed)),
        })
    }
}
