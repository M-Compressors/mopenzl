//! Resource assembly and the encoder's command/output lifecycle.

use super::*;
use crate::Compressor;
use crate::framing::{
    FramedEncodeOperation, FramedEncodeProgress, FramedEncoderStatus, FramedResource, ResourceInput,
};
use alloc::vec::Vec;

#[derive(Debug)]
pub(crate) struct Encoder {
    pending: Vec<u8>,
    cursor: usize,
    entries: Vec<ResourceInfo>,
    offset: u64,
    total: u64,
    finishing: bool,
}

impl Encoder {
    pub(crate) fn start() -> Result<Self, E> {
        let mut pending = Vec::new();
        reserve(&mut pending, MAGIC.len())?;
        pending.extend_from_slice(MAGIC);
        Ok(Self {
            pending,
            cursor: 0,
            entries: Vec::new(),
            offset: 8,
            total: 0,
            finishing: false,
        })
    }

    pub(crate) fn resource(
        &mut self,
        raw: &mut Compressor,
        config: FramingConfig,
        resource: FramedResource<'_>,
    ) -> Result<(), E> {
        if self.finishing {
            return Err(E::InvalidState);
        }
        if self.cursor != self.pending.len() {
            return Err(E::OutputPending);
        }
        limit(
            add(self.entries.len() as u64, 1)?,
            config.max_resources as u64,
            "resource count",
        )?;
        let size = match resource.input {
            ResourceInput::Serial(bytes) => bytes.len() as u64,
            ResourceInput::Typed(inputs) => inputs.iter().try_fold(0, |sum, input| {
                // Input::byte_len includes string length storage; frame size
                // declarations count content only.
                add(sum, input.to_stream().content_size() as u64)
            })?,
        };
        let total = add(self.total, size)?;
        limit(total, config.max_output_bytes, "output size")?;
        self.pending.clear();
        self.cursor = 0;
        reserve(&mut self.pending, RECORD_HEADER)?;
        self.pending.extend_from_slice(b"RSRC");
        self.pending.resize(RECORD_HEADER, 0);
        if let Err(error) = metadata_into(
            resource.metadata,
            config.max_metadata_bytes,
            &mut self.pending,
        ) {
            self.pending.clear();
            self.cursor = 0;
            return Err(error);
        }
        let metadata_len = self.pending.len() - RECORD_HEADER;
        let old_graph = resource
            .graph
            .map(|graph| raw.replace_graph(Some(graph.clone())));
        let frame = match resource.input {
            ResourceInput::Serial(bytes) => raw.compress(bytes),
            ResourceInput::Typed(inputs) => raw.compress_inputs(inputs),
        };
        if let Some(graph) = old_graph {
            raw.replace_graph(graph);
        }
        let result = (|| {
            let frame = frame?;
            limit(
                frame.len() as u64,
                config.max_frame_bytes as u64,
                "frame size",
            )?;
            self.pending[4..8].copy_from_slice(&(metadata_len as u32).to_le_bytes());
            self.pending[8..16].copy_from_slice(&(frame.len() as u64).to_le_bytes());
            self.pending[16..24].copy_from_slice(&size.to_le_bytes());
            let checksum = xxh3::hash32(raw.backend().0, &self.pending);
            let length = add(self.pending.len() as u64 + 4, frame.len() as u64)?;
            let next = add(self.offset, length)?;
            // Reserve space for the mandatory directory and footer as well.
            let end = add(
                next,
                add(
                    index_size(self.entries.len() + 1)? as u64,
                    FOOTER_LEN as u64,
                )?,
            )?;
            limit(end, config.max_container_bytes, "container size")?;
            reserve(
                &mut self.pending,
                frame.len().checked_add(4).ok_or(E::Overflow)?,
            )?;
            reserve(&mut self.entries, 1)?;
            self.pending.extend_from_slice(&checksum.to_le_bytes());
            self.pending.extend_from_slice(&frame);
            self.entries.push(ResourceInfo {
                offset: self.offset,
                length,
                decoded_size: size,
            });
            self.offset = next;
            self.total = total;
            Ok(())
        })();
        self.cursor = 0;
        if result.is_err() {
            self.pending.clear();
        }
        result
    }

    pub(crate) fn process(
        &mut self,
        raw: &Compressor,
        config: FramingConfig,
        output: &mut [u8],
        op: FramedEncodeOperation,
    ) -> Result<FramedEncodeProgress, E> {
        let mut produced = 0;
        loop {
            let take = (self.pending.len() - self.cursor).min(output.len() - produced);
            output[produced..produced + take]
                .copy_from_slice(&self.pending[self.cursor..self.cursor + take]);
            produced += take;
            self.cursor += take;
            if self.cursor != self.pending.len() {
                return Ok(FramedEncodeProgress {
                    produced,
                    status: FramedEncoderStatus::NeedsOutput,
                });
            }
            if self.finishing {
                return Ok(FramedEncodeProgress {
                    produced,
                    status: FramedEncoderStatus::Finished,
                });
            }
            if op == FramedEncodeOperation::Process {
                return Ok(FramedEncodeProgress {
                    produced,
                    status: FramedEncoderStatus::NeedsInput,
                });
            }
            let suffix = suffix(&self.entries, self.offset, raw.backend().0, config)?;
            self.pending = suffix;
            self.cursor = 0;
            self.finishing = true;
        }
    }
}
