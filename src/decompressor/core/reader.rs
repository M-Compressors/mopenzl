//! A cursor over a header that tells truncation apart from corruption.
//!
//! Sessions parse headers over whatever input has arrived; running out of
//! bytes there means "wait for more", not "invalid".

use crate::decompressor::error::{DecodeError, InvalidDataKind};
use crate::shared::bitpack;
use crate::shared::varint::{self, VarintError};

/// Why a header could not be parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Parse {
    /// More bytes are needed.
    Truncated,
    /// The bytes are invalid, or unsupported.
    Failed(DecodeError),
}

impl From<DecodeError> for Parse {
    fn from(error: DecodeError) -> Self {
        Self::Failed(error)
    }
}

/// Reads a header from the front of `bytes`.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// What malformed varints report.
    kind: InvalidDataKind,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8], kind: InvalidDataKind) -> Self {
        Self {
            bytes,
            pos: 0,
            kind,
        }
    }

    /// Bytes read so far.
    pub(crate) const fn pos(&self) -> usize {
        self.pos
    }

    /// The bytes read so far.
    pub(crate) fn consumed(&self) -> &'a [u8] {
        &self.bytes[..self.pos]
    }

    /// The error for invalid data here.
    pub(crate) const fn invalid(&self) -> Parse {
        Parse::Failed(DecodeError::invalid(self.kind))
    }

    pub(crate) fn byte(&mut self) -> Result<u8, Parse> {
        let byte = *self.bytes.get(self.pos).ok_or(Parse::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    pub(crate) fn take(&mut self, len: usize) -> Result<&'a [u8], Parse> {
        let end = self.pos.checked_add(len).ok_or(Parse::Truncated)?;
        let bytes = self.bytes.get(self.pos..end).ok_or(Parse::Truncated)?;
        self.pos = end;
        Ok(bytes)
    }

    pub(crate) fn varint(&mut self) -> Result<u64, Parse> {
        match varint::decode(&self.bytes[self.pos..]) {
            Ok((value, len)) => {
                self.pos += len;
                Ok(value)
            }
            Err(VarintError::Truncated) => Err(Parse::Truncated),
            Err(VarintError::Overlong) => Err(self.invalid()),
        }
    }

    /// Reads `count` packed values of `bits` bits.
    pub(crate) fn packed(
        &mut self,
        count: usize,
        bits: u32,
        sink: impl FnMut(u32),
    ) -> Result<(), Parse> {
        let len =
            bitpack::unpack(&self.bytes[self.pos..], count, bits, sink).ok_or(Parse::Truncated)?;
        self.pos += len;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_out_is_truncation_and_overlong_varints_are_invalid() {
        let mut reader = Reader::new(&[0x81], InvalidDataKind::ChunkHeader);
        assert_eq!(reader.varint(), Err(Parse::Truncated));
        assert_eq!(reader.take(2), Err(Parse::Truncated));
        assert_eq!(reader.byte(), Ok(0x81));
        assert_eq!(reader.byte(), Err(Parse::Truncated));
        assert_eq!(reader.consumed(), [0x81]);
        let overlong = [0xffu8; 10];
        let mut reader = Reader::new(&overlong, InvalidDataKind::FrameHeader);
        assert_eq!(
            reader.varint(),
            Err(Parse::Failed(DecodeError::invalid(
                InvalidDataKind::FrameHeader
            )))
        );
        let mut reader = Reader::new(&[0b101], InvalidDataKind::ChunkHeader);
        let mut seen = alloc::vec::Vec::new();
        reader.packed(3, 1, |v| seen.push(v)).unwrap();
        assert_eq!((seen, reader.pos()), (alloc::vec![1, 0, 1], 1));
        assert_eq!(reader.packed(9, 1, drop), Err(Parse::Truncated));
        assert_eq!(
            Parse::from(DecodeError::InvalidState),
            Parse::Failed(DecodeError::InvalidState)
        );
    }
}
