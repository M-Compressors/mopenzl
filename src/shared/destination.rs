//! The caller's output slice of one session call, initialized or not.
//!
//! Sessions copy pending bytes into whatever the caller offers. A
//! `[MaybeUninit<u8>]` destination is written byte by byte and never read,
//! so no byte of it has to be initialized first.

use core::mem::MaybeUninit;

/// A caller-owned output slice.
pub(crate) trait Destination {
    /// Bytes the slice holds.
    fn capacity(&self) -> usize;
    /// Copies `bytes` to `at`; the caller has checked that they fit.
    fn put(&mut self, at: usize, bytes: &[u8]);
}

impl Destination for [u8] {
    fn capacity(&self) -> usize {
        self.len()
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        self[at..at + bytes.len()].copy_from_slice(bytes);
    }
}

impl Destination for [MaybeUninit<u8>] {
    fn capacity(&self) -> usize {
        self.len()
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        // `write_copy_of_slice` needs Rust 1.93; this compiles to a `memcpy`.
        for (slot, &byte) in self[at..at + bytes.len()].iter_mut().zip(bytes) {
            slot.write(byte);
        }
    }
}

/// Copies as much of `pending` as fits after `at`, returning how much.
pub(crate) fn fill<D: Destination + ?Sized>(output: &mut D, at: usize, pending: &[u8]) -> usize {
    let n = pending.len().min(output.capacity() - at);
    output.put(at, &pending[..n]);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_kinds_receive_the_same_bytes() {
        let mut plain = [0u8; 4];
        assert_eq!(fill(&mut plain[..], 1, b"abcdef"), 3);
        assert_eq!(&plain, b"\0abc");
        let mut uninit = [MaybeUninit::<u8>::uninit(); 4];
        assert_eq!(fill(&mut uninit[..], 1, b"xyz!"), 3);
        assert_eq!(Destination::capacity(&uninit[..]), 4);
        // SAFETY: `fill` wrote bytes 1..4.
        let written: [u8; 3] = core::array::from_fn(|i| unsafe { uninit[1 + i].assume_init() });
        assert_eq!(&written, b"xyz");
    }
}
