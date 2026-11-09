//! `std::io` adapters over compression sessions.

mod reader;
mod writer;

pub use reader::{EncoderReader, EncoderReaderParts};
pub use writer::EncoderWriter;

use std::io::{Read, Write};

use super::encoder::Compressor;
use super::session::StreamConfig;

impl Compressor {
    /// Compresses everything written to the returned adapter into `sink`.
    ///
    /// The frame ends only with [`EncoderWriter::finish`]; dropping the
    /// adapter abandons it. Declare the input size to keep at most one chunk
    /// of input in memory; see [`InputSize`](crate::InputSize).
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::{Compressor, InputSize};
    /// use std::io::Write;
    ///
    /// let mut encoder = Compressor::default();
    /// let payload = b"chunk one chunk two ";
    /// let mut sink = encoder.writer(Vec::new(), InputSize::Exact(payload.len() as u64).into());
    /// for chunk in payload.chunks(4) {
    ///     sink.write_all(chunk)?;
    /// }
    /// let frame = sink.finish().map_err(mopenzl::io::FinishError::into_error)?;
    /// assert!(!frame.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn writer<W: Write>(&mut self, sink: W, stream: StreamConfig) -> EncoderWriter<'_, W> {
        EncoderWriter::new(self.start(stream), sink)
    }

    /// Reads `source` to its end and yields the compressed frame.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::Compressor;
    /// use std::io::Read;
    ///
    /// let mut encoder = Compressor::default();
    /// let mut frame = Vec::new();
    /// encoder.reader(&b"pulled payload"[..], Default::default()).read_to_end(&mut frame)?;
    /// assert!(!frame.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn reader<R: Read>(&mut self, source: R, stream: StreamConfig) -> EncoderReader<'_, R> {
        EncoderReader::new(self.start(stream), source)
    }
}
