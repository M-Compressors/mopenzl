//! Destinations a frame is serialized into.
//!
//! The one-shot paths write straight into the caller's `Vec` or slice; the
//! sessions write into their own pending buffer. The compressed checksum
//! reads back the bytes of the chunk just written, so a sink exposes what it
//! holds.

use alloc::vec::Vec;
use core::mem::MaybeUninit;

use borrowed_buf::BorrowedBuf;

/// The destination has no room for the bytes offered.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Full;

/// An append-only byte destination.
pub(crate) trait Sink {
    /// Bytes written so far.
    fn len(&self) -> usize;
    /// Appends `bytes`, or writes nothing and fails.
    fn put(&mut self, bytes: &[u8]) -> Result<(), Full>;
    /// The bytes written from position `from` on.
    fn since(&self, from: usize) -> &[u8];
}

/// Appends to a `Vec`, counting from where it started.
#[derive(Debug)]
pub(crate) struct VecSink<'a> {
    vec: &'a mut Vec<u8>,
    start: usize,
}

impl<'a> VecSink<'a> {
    pub(crate) fn new(vec: &'a mut Vec<u8>) -> Self {
        let start = vec.len();
        Self { vec, start }
    }
}

impl Sink for VecSink<'_> {
    fn len(&self) -> usize {
        self.vec.len() - self.start
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Full> {
        self.vec.extend_from_slice(bytes);
        Ok(())
    }

    fn since(&self, from: usize) -> &[u8] {
        &self.vec[self.start + from..]
    }
}

/// Fills a caller's slice from its start.
#[derive(Debug)]
pub(crate) struct SliceSink<'a> {
    slice: &'a mut [u8],
    pos: usize,
}

impl<'a> SliceSink<'a> {
    pub(crate) const fn new(slice: &'a mut [u8]) -> Self {
        Self { slice, pos: 0 }
    }
}

impl Sink for SliceSink<'_> {
    fn len(&self) -> usize {
        self.pos
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Full> {
        let end = self.pos.checked_add(bytes.len()).ok_or(Full)?;
        let room = self.slice.get_mut(self.pos..end).ok_or(Full)?;
        room.copy_from_slice(bytes);
        self.pos = end;
        Ok(())
    }

    fn since(&self, from: usize) -> &[u8] {
        &self.slice[from..self.pos]
    }
}

/// Fills a caller's possibly uninitialized slice from its start; only the
/// bytes written become initialized, and nothing is zeroed.
#[derive(Debug)]
pub(crate) struct UninitSink<'a>(BorrowedBuf<'a, u8>);

impl<'a> From<&'a mut [MaybeUninit<u8>]> for UninitSink<'a> {
    fn from(slice: &'a mut [MaybeUninit<u8>]) -> Self {
        Self(BorrowedBuf::from(slice))
    }
}

impl Sink for UninitSink<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Full> {
        if bytes.len() > self.0.capacity() - self.0.len() {
            return Err(Full);
        }
        self.0.unfilled().append(bytes);
        Ok(())
    }

    fn since(&self, from: usize) -> &[u8] {
        &self.0.filled()[from..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_sink_counts_from_its_starting_length() {
        let mut vec = alloc::vec![9u8];
        let mut sink = VecSink::new(&mut vec);
        sink.put(b"ab").unwrap();
        sink.put(b"c").unwrap();
        assert_eq!(sink.len(), 3);
        assert_eq!(sink.since(1), b"bc");
        assert_eq!(vec, [9, b'a', b'b', b'c']);
    }

    #[test]
    fn slice_sink_refuses_what_does_not_fit_without_writing() {
        let mut buffer = [0u8; 4];
        let mut sink = SliceSink::new(&mut buffer);
        sink.put(b"abc").unwrap();
        assert_eq!(sink.put(b"de"), Err(Full));
        assert_eq!(sink.len(), 3);
        assert_eq!(sink.since(0), b"abc");
        assert_eq!(buffer, [b'a', b'b', b'c', 0]);
    }

    #[test]
    fn uninit_sink_initializes_only_what_it_writes() {
        let mut buffer = [MaybeUninit::<u8>::uninit(); 4];
        let mut sink = UninitSink::from(&mut buffer[..]);
        sink.put(b"ab").unwrap();
        assert_eq!(sink.put(b"cde"), Err(Full));
        sink.put(b"c").unwrap();
        assert_eq!((sink.len(), sink.since(1)), (3, &b"bc"[..]));
    }
}
