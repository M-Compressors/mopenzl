//! Incremental units, independent-frame decoding and directory validation.

use super::*;
use crate::framing::{DecodedResource, FramedDecodeProgress, FramedDecoderStatus};
use crate::{DecodeOperation, Decompressor};

pub(crate) fn record_length(header: &[u8], config: FramingConfig) -> Result<usize, E> {
    if header.get(..4) != Some(b"RSRC") {
        return Err(E::Invalid("resource magic"));
    }
    let meta = u32::from_le_bytes(number(header, 4)?) as u64;
    let frame = u64::from_le_bytes(number(header, 8)?);
    limit(meta, config.max_metadata_bytes as u64, "metadata size")?;
    limit(frame, config.max_frame_bytes as u64, "frame size")?;
    let size = u64::from_le_bytes(number(header, 16)?);
    limit(size, config.max_output_bytes, "output size")?;
    host(add(add(RECORD_HEADER as u64 + 4, meta)?, frame)?)
}

pub(crate) fn resource(
    raw: &mut Decompressor,
    bytes: &[u8],
    info: ResourceInfo,
    config: FramingConfig,
) -> Result<DecodedResource, E> {
    let length = record_length(bytes, config)?;
    if length != bytes.len()
        || info.length != length as u64
        || u64::from_le_bytes(number(bytes, 16)?) != info.decoded_size
    {
        return Err(E::Invalid("resource directory mismatch"));
    }
    let meta_len = u32::from_le_bytes(number(bytes, 4)?) as usize;
    let check_at = RECORD_HEADER + meta_len;
    if xxh3::hash32(raw.backend().0, &bytes[..check_at])
        != u32::from_le_bytes(number(bytes, check_at)?)
    {
        return Err(E::Invalid("resource header checksum"));
    }
    let metadata = metadata(&bytes[RECORD_HEADER..check_at])?;
    let frame = &bytes[check_at + 4..];
    // Validate outer size against the inner header before invoking any codec.
    let inner = raw.frame_info(frame)?;
    if inner.decompressed_size() != info.decoded_size {
        return Err(E::Invalid("resource decoded size"));
    }
    let outputs = raw.decompress_typed(frame)?;
    Ok(DecodedResource {
        info,
        metadata,
        outputs,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Header,
    Tag,
    Record,
    Index,
    Footer,
    End,
    Done,
    Failed,
}

#[derive(Debug)]
pub(crate) struct Decoder {
    phase: Phase,
    unit: Vec<u8>,
    target: usize,
    entries: Vec<ResourceInfo>,
    offset: u64,
    total: u64,
    index_offset: u64,
    index_len: u64,
    pub(crate) accepted: u64,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            phase: Phase::Header,
            unit: Vec::new(),
            target: 8,
            entries: Vec::new(),
            offset: 0,
            total: 0,
            index_offset: 0,
            index_len: 0,
            accepted: 0,
        }
    }
}

impl Decoder {
    pub(crate) fn process(
        &mut self,
        raw: &mut Decompressor,
        config: FramingConfig,
        input: &[u8],
        op: DecodeOperation,
    ) -> Result<FramedDecodeProgress, E> {
        if self.phase == Phase::Failed {
            return Err(E::InvalidState);
        }
        let result = self.advance(raw, config, input, op);
        if result.is_err() {
            self.phase = Phase::Failed;
        }
        result
    }

    fn advance(
        &mut self,
        raw: &mut Decompressor,
        config: FramingConfig,
        input: &[u8],
        op: DecodeOperation,
    ) -> Result<FramedDecodeProgress, E> {
        let mut consumed = 0;
        loop {
            if self.phase == Phase::Done {
                if !input.is_empty() {
                    return Err(E::InvalidState);
                }
                return Ok(FramedDecodeProgress {
                    consumed: 0,
                    resource: None,
                    status: FramedDecoderStatus::Finished,
                });
            }
            if self.phase == Phase::End {
                if consumed != input.len() {
                    return Err(E::Invalid("trailing data"));
                }
                if op == DecodeOperation::Finish {
                    self.phase = Phase::Done;
                }
                return Ok(FramedDecodeProgress {
                    consumed,
                    resource: None,
                    status: if op == DecodeOperation::Finish {
                        FramedDecoderStatus::Finished
                    } else {
                        FramedDecoderStatus::NeedsInput
                    },
                });
            }
            let take = (self.target - self.unit.len()).min(input.len() - consumed);
            limit(
                add(self.accepted, take as u64)?,
                config.max_container_bytes,
                "container size",
            )?;
            reserve(&mut self.unit, take)?;
            self.unit
                .extend_from_slice(&input[consumed..consumed + take]);
            consumed += take;
            self.accepted += take as u64;
            if self.unit.len() < self.target {
                if op == DecodeOperation::Finish {
                    return Err(E::UnexpectedEndOfInput);
                }
                return Ok(FramedDecodeProgress {
                    consumed,
                    resource: None,
                    status: FramedDecoderStatus::NeedsInput,
                });
            }
            match self.phase {
                Phase::Header => {
                    check_magic(&self.unit)?;
                    self.offset = 8;
                    self.next(Phase::Tag, 4);
                }
                Phase::Tag => match self.unit.as_slice() {
                    b"RSRC" => {
                        limit(
                            add(self.entries.len() as u64, 1)?,
                            config.max_resources as u64,
                            "resource count",
                        )?;
                        self.phase = Phase::Record;
                        self.target = RECORD_HEADER;
                    }
                    b"INDX" => {
                        self.index_offset = self.offset;
                        self.phase = Phase::Index;
                        self.target = 8;
                    }
                    _ => return Err(E::Invalid("record tag")),
                },
                Phase::Record => {
                    let len = record_length(&self.unit, config)?;
                    let size = u64::from_le_bytes(number(&self.unit, 16)?);
                    limit(
                        add(self.total, size)?,
                        config.max_output_bytes,
                        "output size",
                    )?;
                    limit(
                        add(self.offset, len as u64)?,
                        config.max_container_bytes,
                        "container size",
                    )?;
                    if self.unit.len() < len {
                        self.target = len;
                        continue;
                    }
                    let info = ResourceInfo {
                        offset: self.offset,
                        length: len as u64,
                        decoded_size: size,
                    };
                    reserve(&mut self.entries, 1)?;
                    let output = resource(raw, &self.unit, info, config)?;
                    self.entries.push(info);
                    self.offset = add(self.offset, len as u64)?;
                    self.total = add(self.total, size)?;
                    self.next(Phase::Tag, 4);
                    return Ok(FramedDecodeProgress {
                        consumed,
                        resource: Some(output),
                        status: FramedDecoderStatus::Resource,
                    });
                }
                Phase::Index => {
                    let count = u32::from_le_bytes(number(&self.unit, 4)?) as usize;
                    limit(count as u64, config.max_resources as u64, "resource count")?;
                    let len = index_size(count)?;
                    limit(
                        add(self.offset, add(len as u64, FOOTER_LEN as u64)?)?,
                        config.max_container_bytes,
                        "container size",
                    )?;
                    if self.unit.len() < len {
                        self.target = len;
                        continue;
                    }
                    let parsed = index(&self.unit, self.index_offset, raw.backend().0, config)?;
                    if parsed != self.entries {
                        return Err(E::Invalid("directory disagrees with resources"));
                    }
                    self.index_len = len as u64;
                    self.offset = add(self.offset, len as u64)?;
                    self.next(Phase::Footer, FOOTER_LEN);
                }
                Phase::Footer => {
                    self.offset = add(self.offset, FOOTER_LEN as u64)?;
                    footer(&self.unit, self.index_offset, self.index_len, self.offset)?;
                    self.next(Phase::End, 0);
                }
                Phase::End | Phase::Done | Phase::Failed => return Err(E::InvalidState),
            }
        }
    }

    fn next(&mut self, phase: Phase, target: usize) {
        self.phase = phase;
        self.target = target;
        self.unit.clear();
    }
}
