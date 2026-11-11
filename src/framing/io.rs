//! Blocking transports over the same resource and directory mechanics.

use super::*;
#[cfg(feature = "compression")]
use alloc::boxed::Box;
#[cfg(feature = "compression")]
use std::io::Write;
#[cfg(feature = "decompression")]
use std::io::{Read, Seek, SeekFrom};

/// Failed finalization with the complete writer retained for a transport retry.
#[cfg(feature = "compression")]
pub struct FramedFinishError<T> {
    /// The failure that stopped finalization.
    pub error: FramedError,
    /// Writer, including all undelivered bytes.
    pub writer: T,
}

#[cfg(feature = "compression")]
impl<T> std::fmt::Debug for FramedFinishError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FramedFinishError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "compression")]
impl<T> std::fmt::Display for FramedFinishError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "framed container could not be finished: {}", self.error)
    }
}

#[cfg(feature = "compression")]
impl<T> std::error::Error for FramedFinishError<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Resource-oriented writer. A successful resource command queues its complete
/// frame; flush, the next resource command, or finish delivers it. No bytes are
/// written on Drop. Partial, interrupted and WouldBlock writes preserve cursors.
#[cfg(feature = "compression")]
#[derive(Debug)]
pub struct FramedWriter<'a, W> {
    session: FramedEncoderSession<'a>,
    sink: W,
    buffer: [u8; 8192],
    start: usize,
    end: usize,
    finished: bool,
}

#[cfg(feature = "compression")]
impl FramedCompressor {
    /// Starts a container on a writer. The header is queued, not written yet.
    ///
    /// # Errors
    /// A framing allocation failed.
    ///
    /// # Examples
    /// ```
    /// use mopenzl::framing::{FramedCompressor, FramedResource};
    /// let mut owner = FramedCompressor::new(Default::default())?;
    /// let mut writer = owner.framed_writer(Vec::new())?;
    /// writer.resource(FramedResource::from(&b"payload"[..]))?;
    /// let bytes = writer.finish().map_err(|e| e.error)?;
    /// assert!(bytes.starts_with(b"MOZFRM"));
    /// # Ok::<(), mopenzl::framing::FramedError>(())
    /// ```
    pub fn framed_writer<W: Write>(&mut self, sink: W) -> Result<FramedWriter<'_, W>, FramedError> {
        Ok(FramedWriter {
            session: self.start()?,
            sink,
            buffer: [0; 8192],
            start: 0,
            end: 0,
            finished: false,
        })
    }
}

#[cfg(feature = "compression")]
impl<W: Write> FramedWriter<'_, W> {
    /// Drains older bytes, then queues this resource. On error this resource
    /// was not accepted, so the same command can safely be retried.
    ///
    /// # Errors
    /// Transport failures before acceptance; otherwise as the native command.
    pub fn resource(&mut self, resource: FramedResource<'_>) -> Result<(), FramedError> {
        if self.finished {
            return Err(FramedError::InvalidState);
        }
        self.drain(FramedEncodeOperation::Process)?;
        self.session.resource(resource)
    }

    /// Delivers the queued header/resource and flushes the sink.
    ///
    /// # Errors
    /// Underlying transport failure; retry preserves the unwritten suffix.
    pub fn flush(&mut self) -> Result<(), FramedError> {
        self.drain(FramedEncodeOperation::Process)?;
        self.sink.flush()?;
        Ok(())
    }

    /// Completes the directory/footer and flushes, retaining self on failure.
    ///
    /// # Errors
    /// Allocation, size or transport failures; transport failures are retryable.
    pub fn try_finish(&mut self) -> Result<(), FramedError> {
        self.drain(FramedEncodeOperation::Finish)?;
        self.sink.flush()?;
        self.finished = true;
        Ok(())
    }

    /// Completes the container and returns the sink, or the whole writer.
    ///
    /// # Errors
    /// As [`Self::try_finish`].
    pub fn finish(mut self) -> Result<W, Box<FramedFinishError<Self>>> {
        match self.try_finish() {
            Ok(()) => Ok(self.sink),
            Err(error) => Err(Box::new(FramedFinishError {
                error,
                writer: self,
            })),
        }
    }

    fn drain(&mut self, op: FramedEncodeOperation) -> Result<(), FramedError> {
        loop {
            while self.start < self.end {
                match self.sink.write(&self.buffer[self.start..self.end]) {
                    Ok(0) => return Err(FramedError::Io(std::io::ErrorKind::WriteZero.into())),
                    Ok(n) if n <= self.end - self.start => self.start += n,
                    Ok(_) => {
                        return Err(FramedError::Invalid("writer returned an oversized count"));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            let progress = self.session.process(&mut self.buffer, op)?;
            self.start = 0;
            self.end = progress.produced;
            if self.end == 0 && progress.status != FramedEncoderStatus::NeedsOutput {
                return Ok(());
            }
        }
    }
}

/// Sequential resource reader, buffering at most one encoded resource, decoded
/// resource, the directory, and 8 KiB of transport input. Keep reading through
/// `None` to validate the directory, footer and EOF.
#[cfg(feature = "decompression")]
#[derive(Debug)]
pub struct FramedReader<'a, R> {
    session: FramedDecoderSession<'a>,
    source: R,
    buffer: [u8; 8192],
    start: usize,
    end: usize,
    eof: bool,
    failed: bool,
}

#[cfg(feature = "decompression")]
impl FramedDecompressor {
    /// Reads resource events from a source. No bytes are read until requested.
    pub fn framed_reader<R: Read>(&mut self, source: R) -> FramedReader<'_, R> {
        FramedReader {
            session: self.start(),
            source,
            buffer: [0; 8192],
            start: 0,
            end: 0,
            eof: false,
            failed: false,
        }
    }

    /// Loads and validates the seek index from the current source position to
    /// its end. Only the header, directory and footer are read; individual
    /// resource headers/checksums and frames are validated when requested.
    ///
    /// # Errors
    /// Invalid/truncated index, configured limits, allocation or transport errors.
    pub fn framed_seek_reader<R: Read + Seek>(
        &mut self,
        mut source: R,
    ) -> Result<FramedSeekReader<'_, R>, FramedError> {
        use super::core::{FOOTER_LEN, MAGIC};
        let base = source.stream_position()?;
        let end = source.seek(SeekFrom::End(0))?;
        let total = end
            .checked_sub(base)
            .ok_or(FramedError::Invalid("container range"))?;
        core::limit(total, self.config.max_container_bytes, "container size")?;
        if total < (MAGIC.len() + 12 + FOOTER_LEN) as u64 {
            return Err(FramedError::UnexpectedEndOfInput);
        }
        source.seek(SeekFrom::Start(base))?;
        let mut header = [0; 8];
        source.read_exact(&mut header)?;
        core::check_magic(&header)?;
        source.seek(SeekFrom::Start(end - FOOTER_LEN as u64))?;
        let mut footer = [0; FOOTER_LEN];
        source.read_exact(&mut footer)?;
        let offset = u64::from_le_bytes(core::number(&footer, 8)?);
        let len = u64::from_le_bytes(core::number(&footer, 16)?);
        core::footer(&footer, offset, len, total)?;
        if offset < 8 || core::add(core::add(offset, len)?, FOOTER_LEN as u64)? != total {
            return Err(FramedError::Invalid("directory range"));
        }
        core::limit(
            len,
            core::index_size(self.config.max_resources)? as u64,
            "directory size",
        )?;
        let mut directory = Vec::new();
        let host_len = core::host(len)?;
        core::reserve(&mut directory, host_len)?;
        directory.resize(host_len, 0);
        source.seek(SeekFrom::Start(core::add(base, offset)?))?;
        source.read_exact(&mut directory)?;
        let entries = core::index(&directory, offset, self.raw.backend().0, self.config)?;
        Ok(FramedSeekReader {
            owner: self,
            source,
            base,
            entries,
        })
    }
}

#[cfg(feature = "decompression")]
impl<R: Read> FramedReader<'_, R> {
    /// Returns one resource, or None after validation of the complete container.
    ///
    /// # Errors
    /// Container/codec failures are sticky. Transport errors preserve state and
    /// may be retried, including WouldBlock and Interrupted.
    pub fn read_resource(&mut self) -> Result<Option<DecodedResource>, FramedError> {
        if self.failed {
            return Err(FramedError::InvalidState);
        }
        loop {
            if self.start == self.end && !self.eof {
                match self.source.read(&mut self.buffer) {
                    Ok(n) if n <= self.buffer.len() => {
                        self.start = 0;
                        self.end = n;
                        self.eof = n == 0;
                    }
                    Ok(_) => {
                        self.failed = true;
                        return Err(FramedError::Invalid("reader returned an oversized count"));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            let op = if self.eof {
                crate::DecodeOperation::Finish
            } else {
                crate::DecodeOperation::Process
            };
            let progress = match self.session.process(&self.buffer[self.start..self.end], op) {
                Ok(p) => p,
                Err(failure) => {
                    self.failed = true;
                    return Err(failure.error);
                }
            };
            self.start += progress.consumed;
            if let Some(resource) = progress.resource {
                return Ok(Some(resource));
            }
            if progress.status == FramedDecoderStatus::Finished {
                return Ok(None);
            }
        }
    }
}

/// Indexed access to independent resources. Selecting one never decodes earlier
/// resources; external OpenZL dictionary bundles still come from the owner's
/// configured BundleStore. Offsets are relative even for embedded containers.
#[cfg(feature = "decompression")]
#[derive(Debug)]
pub struct FramedSeekReader<'a, R> {
    owner: &'a mut FramedDecompressor,
    source: R,
    base: u64,
    entries: Vec<ResourceInfo>,
}

#[cfg(feature = "decompression")]
impl<R: Read + Seek> FramedSeekReader<'_, R> {
    /// Validated directory in resource order; payloads have not been decoded.
    pub fn resources(&self) -> &[ResourceInfo] {
        &self.entries
    }

    /// Reads and decodes exactly the selected resource, validating its header,
    /// metadata, size, inner frame and configured codec checksums.
    ///
    /// # Errors
    /// Invalid index, record, frame, missing dictionary, allocation or I/O errors.
    pub fn read_resource(&mut self, index: usize) -> Result<DecodedResource, FramedError> {
        let info = *self
            .entries
            .get(index)
            .ok_or(FramedError::Invalid("resource index"))?;
        self.source
            .seek(SeekFrom::Start(core::add(self.base, info.offset)?))?;
        let mut header = [0; core::RECORD_HEADER];
        self.source.read_exact(&mut header)?;
        let len = core::decode::record_length(&header, self.owner.config)?;
        if len as u64 != info.length {
            return Err(FramedError::Invalid("resource directory mismatch"));
        }
        let mut record = Vec::new();
        core::reserve(&mut record, len)?;
        record.extend_from_slice(&header);
        record.resize(len, 0);
        self.source.read_exact(&mut record[header.len()..])?;
        core::decode::resource(&mut self.owner.raw, &record, info, self.owner.config)
    }
}
