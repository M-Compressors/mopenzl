//! The standard segmenters (`segmenter_serial.c`, `segmenter_numeric.c`).

use super::super::error::ExecError;
use super::super::exec::SegmenterCx;
use super::super::params::LocalParams;
use super::super::registry::{GraphId, SegmentFn, gid};

/// `ZL_SEGMENT_SERIAL_CHUNK_BYTE_SIZE_PARAM`.
pub(crate) const SERIAL_CHUNK_BYTE_SIZE: i32 =
    crate::graph::params::segmenters::SERIAL_CHUNK_BYTE_SIZE;
/// `SEGM_NUM_FROM_SERIAL_CHUNK_BYTE_SIZE_ID`.
pub(crate) const NUM_FROM_SERIAL_CHUNK_BYTE_SIZE: i32 =
    crate::graph::params::segmenters::NUM_FROM_SERIAL_CHUNK_BYTE_SIZE;
/// `SEGM_NUM_FROM_SERIAL_ELT_WIDTH_ID`.
pub(crate) const NUM_FROM_SERIAL_ELT_WIDTH: i32 =
    crate::graph::params::segmenters::NUM_FROM_SERIAL_ELT_WIDTH;
/// `ZL_DEFAULT_SEGMENTER_CHUNK_BYTE_SIZE`.
pub(crate) const DEFAULT_CHUNK_BYTE_SIZE: usize = 16 << 20;
/// `ZL_CHUNK_VERSION_MIN`: older formats cannot be segmented.
const CHUNK_VERSION_MIN: u32 = 21;

/// `SEGM_serial`: cuts the input every N bytes.
#[derive(Debug)]
pub(crate) struct SegmentSerial;

/// `SEGM_numeric`: cuts a numeric input every 16 MiB.
#[derive(Debug)]
pub(crate) struct SegmentNumeric;

/// `SEGM_numFromSerial`: cuts serial input every N bytes, aligned down to the
/// element width.
#[derive(Debug)]
pub(crate) struct SegmentNumFromSerial;

/// See [`SegmentSerial`].
pub(crate) static SEGMENT_SERIAL: SegmentSerial = SegmentSerial;
/// See [`SegmentNumeric`].
pub(crate) static SEGMENT_NUMERIC: SegmentNumeric = SegmentNumeric;
/// See [`SegmentNumFromSerial`].
pub(crate) static SEGMENT_NUM_FROM_SERIAL: SegmentNumFromSerial = SegmentNumFromSerial;

/// Feeds `total` units to the successor in pieces of at most `piece`.
fn cut(
    cx: &mut SegmenterCx<'_, '_, '_>,
    total: usize,
    piece: usize,
    successor: GraphId,
) -> Result<(), ExecError> {
    let mut left = total;
    while left > piece {
        cx.process_chunk(&[piece], successor, None)?;
        left -= piece;
    }
    cx.process_chunk(&[left], successor, None)
}

impl SegmentFn for SegmentSerial {
    fn serial_chunk(
        &self,
        params: &LocalParams,
        custom_graphs: &[GraphId],
    ) -> Option<(usize, GraphId)> {
        let size = match params.int(SERIAL_CHUNK_BYTE_SIZE) {
            Some(value) => usize::try_from(value).ok()?,
            None => DEFAULT_CHUNK_BYTE_SIZE,
        };
        (size > 0).then_some(())?;
        Some((size, *custom_graphs.first()?))
    }

    fn segment(&self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError> {
        let input = cx.input(0).ok_or(ExecError::GraphInvalid)?;
        let size = match cx.params.int(SERIAL_CHUNK_BYTE_SIZE) {
            Some(value) => {
                usize::try_from(value).map_err(|_| ExecError::Parameter("negative chunk size"))?
            }
            None => DEFAULT_CHUNK_BYTE_SIZE,
        };
        if size == 0 {
            return Err(ExecError::Parameter("zero chunk size"));
        }
        let successor = *cx
            .custom_graphs
            .first()
            .ok_or(ExecError::Parameter("the segmenter has no successor"))?;
        let total = input.content_size();
        if cx.format_version() < CHUNK_VERSION_MIN {
            return cx.process_chunk(&[total], successor, None);
        }
        cut(cx, total, size, successor)
    }
}

impl SegmentFn for SegmentNumeric {
    fn segment(&self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError> {
        let input = cx.input(0).ok_or(ExecError::GraphInvalid)?;
        let per_chunk = DEFAULT_CHUNK_BYTE_SIZE / input.width().max(1);
        let total = input.n_elts();
        if cx.format_version() < CHUNK_VERSION_MIN {
            return cx.process_chunk(&[total], gid::NUMERIC_COMPRESS, None);
        }
        cut(cx, total, per_chunk, gid::NUMERIC_COMPRESS)
    }
}

impl SegmentFn for SegmentNumFromSerial {
    fn segment(&self, cx: &mut SegmenterCx<'_, '_, '_>) -> Result<(), ExecError> {
        let input = cx.input(0).ok_or(ExecError::GraphInvalid)?;
        let width = cx
            .params
            .int(NUM_FROM_SERIAL_ELT_WIDTH)
            .map_or(1, |v| v as usize);
        if !matches!(width, 1 | 2 | 4 | 8) {
            return Err(ExecError::Parameter("invalid element width"));
        }
        let size = cx
            .params
            .int(NUM_FROM_SERIAL_CHUNK_BYTE_SIZE)
            .map_or(DEFAULT_CHUNK_BYTE_SIZE, |v| v as usize);
        let aligned = size - size % width;
        if aligned == 0 {
            return Err(ExecError::Parameter("chunk size below the element width"));
        }
        let successor = *cx
            .custom_graphs
            .first()
            .ok_or(ExecError::Parameter("the segmenter has no successor"))?;
        let total = input.content_size();
        if cx.format_version() < CHUNK_VERSION_MIN {
            return cx.process_chunk(&[total], successor, None);
        }
        cut(cx, total, aligned, successor)
    }
}
