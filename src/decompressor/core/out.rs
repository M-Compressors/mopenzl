//! Destinations decoded chunks are written into.

use alloc::vec::Vec;
use core::mem::MaybeUninit;

use borrowed_buf::BorrowedBuf;

#[cfg(feature = "zstd")]
use super::zstd::ZstdDecoder;
#[cfg(feature = "zstd")]
use crate::decompressor::error::DecodeError;
#[cfg(feature = "zstd")]
use mzstd::dictionary::DecodeDictionary;

/// The destination has no room for a chunk's output.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Full;

/// A growable byte destination.
pub(crate) trait Out {
    /// Bytes written so far.
    fn len(&self) -> usize;
    /// Extends the output by `n` bytes and returns them for writing.
    fn grow(&mut self, n: usize) -> Result<&mut [u8], Full>;
    /// The bytes from position `from` on.
    fn since(&self, from: usize) -> &[u8];

    /// Appends `bytes`.
    fn put(&mut self, bytes: &[u8]) -> Result<(), Full> {
        self.grow(bytes.len())?.copy_from_slice(bytes);
        Ok(())
    }

    /// Appends the `size` bytes the magicless Zstandard `frame` decodes to.
    #[cfg(feature = "zstd")]
    fn zstd(
        &mut self,
        zstd: &mut ZstdDecoder,
        frame: &[u8],
        size: usize,
        dict: Option<&DecodeDictionary>,
    ) -> Result<(), DecodeError> {
        let written = self.len();
        let dst = self
            .grow(size)
            .map_err(|_| DecodeError::OutputTooSmall { written })?;
        zstd.decode(frame, dst, dict)
    }
}

/// Appends to a `Vec`, counting from where it started.
#[derive(Debug)]
pub(crate) struct VecOut<'a> {
    vec: &'a mut Vec<u8>,
    start: usize,
}

impl<'a> VecOut<'a> {
    pub(crate) fn new(vec: &'a mut Vec<u8>) -> Self {
        let start = vec.len();
        Self { vec, start }
    }
}

impl Out for VecOut<'_> {
    fn len(&self) -> usize {
        self.vec.len() - self.start
    }

    fn grow(&mut self, n: usize) -> Result<&mut [u8], Full> {
        let old = self.vec.len();
        let new = old.checked_add(n).ok_or(Full)?;
        self.vec.try_reserve(n).map_err(|_| Full)?;
        self.vec.resize(new, 0);
        Ok(&mut self.vec[old..])
    }

    fn since(&self, from: usize) -> &[u8] {
        &self.vec[self.start + from..]
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Full> {
        self.vec.try_reserve(bytes.len()).map_err(|_| Full)?;
        self.vec.extend_from_slice(bytes);
        Ok(())
    }

    /// Lets the decoder append, so the destination is never zero-filled.
    #[cfg(feature = "zstd")]
    fn zstd(
        &mut self,
        zstd: &mut ZstdDecoder,
        frame: &[u8],
        size: usize,
        dict: Option<&DecodeDictionary>,
    ) -> Result<(), DecodeError> {
        zstd.decode_append(frame, self.vec, size, dict)
    }
}

/// Fills a caller's slice from its start.
#[derive(Debug)]
pub(crate) struct SliceOut<'a> {
    slice: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceOut<'a> {
    pub(crate) const fn new(slice: &'a mut [u8]) -> Self {
        Self { slice, pos: 0 }
    }
}

impl Out for SliceOut<'_> {
    fn len(&self) -> usize {
        self.pos
    }

    fn grow(&mut self, n: usize) -> Result<&mut [u8], Full> {
        let end = self.pos.checked_add(n).ok_or(Full)?;
        let start = self.pos;
        let room = self.slice.get_mut(start..end).ok_or(Full)?;
        self.pos = end;
        Ok(room)
    }

    fn since(&self, from: usize) -> &[u8] {
        &self.slice[from..self.pos]
    }
}

/// Fills a caller's possibly uninitialized slice from its start. Stored
/// streams are copied without zeroing; a codec's output is zeroed once just
/// ahead of being decoded into.
#[derive(Debug)]
pub(crate) struct UninitOut<'a>(BorrowedBuf<'a, u8>);

impl<'a> From<&'a mut [MaybeUninit<u8>]> for UninitOut<'a> {
    fn from(slice: &'a mut [MaybeUninit<u8>]) -> Self {
        Self(BorrowedBuf::from(slice))
    }
}

impl Out for UninitOut<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn grow(&mut self, n: usize) -> Result<&mut [u8], Full> {
        let old = self.0.len();
        if n > self.0.capacity() - old {
            return Err(Full);
        }
        self.0.unfilled().append_fill(n, 0);
        Ok(&mut self.0.filled_mut()[old..])
    }

    fn since(&self, from: usize) -> &[u8] {
        &self.0.filled()[from..]
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Full> {
        if bytes.len() > self.0.capacity() - self.0.len() {
            return Err(Full);
        }
        self.0.unfilled().append(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_out_grows_from_its_start() {
        let mut vec = alloc::vec![1u8];
        let mut out = VecOut::new(&mut vec);
        out.grow(2).unwrap().copy_from_slice(b"ab");
        assert_eq!((out.len(), out.since(1)), (2, &b"b"[..]));
        assert_eq!(out.grow(usize::MAX), Err(Full));
        assert_eq!(vec, [1, b'a', b'b']);
    }

    #[test]
    fn every_out_appends_and_refuses_overflow() {
        let mut storage = [MaybeUninit::<u8>::uninit(); 5];
        let mut uninit = UninitOut::from(&mut storage[..]);
        uninit.put(b"ab").unwrap();
        uninit.grow(2).unwrap().copy_from_slice(b"cd");
        assert_eq!(uninit.put(b"ef"), Err(Full));
        assert_eq!(uninit.grow(2), Err(Full));
        assert_eq!((uninit.len(), uninit.since(1)), (4, &b"bcd"[..]));
        let mut vec = Vec::new();
        let mut out = VecOut::new(&mut vec);
        out.put(b"xy").unwrap();
        let mut slice = [0u8; 2];
        let mut out = SliceOut::new(&mut slice);
        out.put(b"z").unwrap();
        assert_eq!(out.put(b"zz"), Err(Full));
        assert_eq!(vec, b"xy");
    }

    #[test]
    fn slice_out_refuses_overflow() {
        let mut buffer = [0u8; 3];
        let mut out = SliceOut::new(&mut buffer);
        out.grow(2).unwrap().copy_from_slice(b"ab");
        assert_eq!(out.grow(2), Err(Full));
        assert_eq!(out.since(0), b"ab");
        assert_eq!(out.len(), 2);
    }
}
