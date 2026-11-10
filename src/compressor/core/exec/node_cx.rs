//! The context of one node run (`ZL_Encoder`, enc_interface.c).

use alloc::vec::Vec;

use super::super::Params;
use super::super::data::{Bytes, Stream, Type};
use super::super::error::NodeError;
use super::super::params::LocalParams;
use super::super::registry::NodeSpec;
use super::super::registry::dict::Prepared;
#[cfg(feature = "zstd")]
use super::super::zstd::ZstdNode;

/// What a node encoder sees of the engine: its parameters, the global
/// parameters, and the means to create its outputs and its codec header.
///
/// Outputs are collected here and entered into the runtime graph once the
/// node returns: singleton outcomes into the slots reserved for them, variable
/// outcomes appended in the order they were created.
pub(crate) struct NodeCx<'x, 'a> {
    spec: &'static NodeSpec,
    local: Option<&'x LocalParams>,
    #[cfg(any(
        feature = "zstd",
        feature = "lz4",
        feature = "bitunpack",
        feature = "dispatch",
        feature = "field-lz",
        feature = "lz"
    ))]
    params: &'x Params,
    #[cfg(feature = "zstd")]
    pub(crate) zstd: &'x mut ZstdNode,
    /// The node's materialized dictionary (`ZL_Encoder_getMaterializedDict`).
    pub(crate) dictionary: Option<&'x Prepared>,
    /// The SIMD level the compressor selected once; kernels dispatch on it
    /// outside their inner loops.
    pub(crate) simd: fearless_simd::Level,
    singles: Vec<Option<Stream<'a>>>,
    variable: Vec<(usize, Stream<'a>)>,
    header: Option<Vec<u8>>,
}

/// What a finished node run produced.
pub(crate) struct NodeOutputs<'a> {
    /// The singleton outputs, all filled.
    pub(crate) singles: Vec<Stream<'a>>,
    /// The variable outputs as `(outcome, stream)` in creation order.
    pub(crate) variable: Vec<(usize, Stream<'a>)>,
    /// The codec header, if one was sent.
    pub(crate) header: Option<Vec<u8>>,
}

/// The longest codec header a node may send in total.
const HEADER_LIMIT: usize = 1_000_000;

impl<'x, 'a> NodeCx<'x, 'a> {
    pub(crate) fn new(
        spec: &'static NodeSpec,
        local: Option<&'x LocalParams>,
        #[cfg_attr(
            not(any(
                feature = "zstd",
                feature = "lz4",
                feature = "bitunpack",
                feature = "dispatch",
                feature = "field-lz",
                feature = "lz"
            )),
            expect(
                unused_variables,
                reason = "only the nodes that read levels or the format version use them"
            )
        )]
        params: &'x Params,
        #[cfg(feature = "zstd")] zstd: &'x mut ZstdNode,
    ) -> Self {
        Self {
            spec,
            local,
            #[cfg(any(
                feature = "zstd",
                feature = "lz4",
                feature = "bitunpack",
                feature = "dispatch",
                feature = "field-lz",
                feature = "lz"
            ))]
            params,
            #[cfg(feature = "zstd")]
            zstd,
            dictionary: None,
            simd: crate::Backend::default().0,
            singles: (0..spec.singletons.len()).map(|_| None).collect(),
            variable: Vec::new(),
            header: None,
        }
    }

    /// The node being run.
    pub(crate) const fn spec(&self) -> &'static NodeSpec {
        self.spec
    }

    /// `ZL_CParam_compressionLevel`.
    #[cfg(any(
        feature = "zstd",
        feature = "lz4",
        feature = "field-lz",
        feature = "lz"
    ))]
    pub(crate) const fn level(&self) -> i32 {
        self.params.level
    }

    /// `ZL_CParam_decompressionLevel`.
    #[cfg(feature = "zstd")]
    pub(crate) const fn decompression_level(&self) -> i32 {
        self.params.decompression_level
    }

    /// `ZL_Encoder_getLocalIntParam`: the node's own parameters replace its
    /// defaults when it was parameterized.
    pub(crate) fn int(&self, id: i32) -> Option<i32> {
        match self.local {
            Some(local) => local.int(id),
            None => self
                .spec
                .defaults
                .iter()
                .find(|&&(key, _)| key == id)
                .map(|&(_, value)| value),
        }
    }

    /// All integer parameters in order (`ZL_Encoder_getLocalIntParams`).
    #[cfg(any(feature = "zstd", feature = "lz"))]
    pub(crate) fn ints(&self) -> &[(i32, i32)] {
        match self.local {
            Some(local) => &local.ints[..],
            None => self.spec.defaults,
        }
    }

    /// `ZL_Encoder_getLocalCopyParam`.
    pub(crate) fn copy_param(&self, id: i32) -> Option<&[u8]> {
        self.local?.copy(id)
    }

    /// `ZL_Encoder_getLocalParam`: a reference parameter.
    pub(crate) fn ref_param(&self, id: i32) -> Option<&(dyn core::any::Any + Send + Sync)> {
        self.local?.reference(id)
    }

    fn place(&mut self, outcome: usize, stream: Stream<'a>) -> Result<usize, NodeError> {
        let ty = self
            .spec
            .output_type(outcome)
            .ok_or(NodeError::OutcomeOutOfRange)?;
        if stream.ty() != ty {
            return Err(NodeError::InvalidInput(
                "output type does not match its outcome",
            ));
        }
        let width_ok = match ty {
            Type::Serial | Type::String => stream.width() == 1,
            Type::Numeric => matches!(stream.width(), 1 | 2 | 4 | 8),
            Type::Struct => stream.width() > 0,
        };
        if !width_ok {
            return Err(NodeError::InvalidInput("invalid element width"));
        }
        match self.singles.get_mut(outcome) {
            Some(slot) if slot.is_some() => Err(NodeError::OutputTwice),
            Some(slot) => {
                *slot = Some(stream);
                Ok(outcome)
            }
            None => {
                self.variable.push((outcome, stream));
                Ok(self.singles.len() + self.variable.len() - 1)
            }
        }
    }

    /// `ZL_Encoder_createTypedStream` with its content already written:
    /// outcome `outcome` becomes `bytes` of element width `width`.
    ///
    /// Returns the output's index among the node's outputs.
    ///
    /// # Errors
    ///
    /// An outcome out of range or created twice, or an invalid width.
    pub(crate) fn emit(
        &mut self,
        outcome: usize,
        width: usize,
        bytes: Vec<u8>,
    ) -> Result<usize, NodeError> {
        let ty = self
            .spec
            .output_type(outcome)
            .ok_or(NodeError::OutcomeOutOfRange)?;
        self.place(outcome, Stream::new(ty, width, Bytes::from(bytes)))
    }

    /// `ENC_refTypedStream`: an output that references `n_elts` elements of
    /// `src` starting `offset_bytes` into it, without copying.
    ///
    /// # Errors
    ///
    /// A reference outside `src`, or the errors of [`Self::emit`].
    pub(crate) fn reference(
        &mut self,
        outcome: usize,
        src: &Stream<'a>,
        width: usize,
        n_elts: usize,
        offset_bytes: usize,
    ) -> Result<usize, NodeError> {
        let ty = self
            .spec
            .output_type(outcome)
            .ok_or(NodeError::OutcomeOutOfRange)?;
        let len = width
            .checked_mul(n_elts)
            .ok_or(NodeError::InvalidInput("reference too large"))?;
        let bytes = src
            .content()
            .window(offset_bytes, len)
            .ok_or(NodeError::InvalidInput("reference outside its source"))?;
        self.place(outcome, Stream::new(ty, width, bytes))
    }

    /// A string output that references all of `src` with the given lengths.
    ///
    /// # Errors
    ///
    /// Lengths that do not sum to the size of `src`, or the errors of
    /// [`Self::emit`].
    pub(crate) fn reference_strings(
        &mut self,
        outcome: usize,
        src: &Stream<'a>,
        lens: Vec<u32>,
    ) -> Result<usize, NodeError> {
        let total: u64 = lens.iter().map(|&len| u64::from(len)).sum();
        if total != src.content_size() as u64 {
            return Err(NodeError::InvalidInput(
                "string lengths do not match the content",
            ));
        }
        self.place(outcome, Stream::strings_from(src.content().clone(), lens))
    }

    /// `ZL_Encoder_sendCodecHeader`: the private header the decoder reads;
    /// at most once per run.
    ///
    /// # Errors
    ///
    /// A second header, or one beyond the limit.
    pub(crate) fn send_header(&mut self, header: &[u8]) -> Result<(), NodeError> {
        if self.header.is_some() || header.len() > HEADER_LIMIT {
            return Err(NodeError::Header);
        }
        self.header = Some(header.to_vec());
        Ok(())
    }

    // ---- Entropy package: output metadata ----

    /// [`Self::emit`] followed by `ZL_Output_setIntMetadata` for each pair of
    /// `metadata`: the integer metadata downstream graphs may read (the FSE
    /// and Huffman table streams carry their table log as metadata 0).
    ///
    /// # Errors
    ///
    /// The errors of [`Self::emit`].
    #[cfg(feature = "entropy")]
    pub(crate) fn emit_with_metadata(
        &mut self,
        outcome: usize,
        width: usize,
        bytes: Vec<u8>,
        metadata: &[(i32, i32)],
    ) -> Result<usize, NodeError> {
        let ty = self
            .spec
            .output_type(outcome)
            .ok_or(NodeError::OutcomeOutOfRange)?;
        let mut stream = Stream::new(ty, width, Bytes::from(bytes));
        for &(id, value) in metadata {
            stream.set_metadata(id, value);
        }
        self.place(outcome, stream)
    }

    /// Ends the run: every singleton must have been created.
    pub(crate) fn finish(self) -> Result<NodeOutputs<'a>, NodeError> {
        let singles = self
            .singles
            .into_iter()
            .map(|slot| slot.ok_or(NodeError::OutputNotCommitted))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(NodeOutputs {
            singles,
            variable: self.variable,
            header: self.header,
        })
    }

    // Engine additions of the LZ package: kept together at the end.

    /// `ZL_Output_setIntMetadata`: attaches the integer metadata `id` to the
    /// singleton output `outcome`, which must already have been created.
    ///
    /// # Errors
    ///
    /// An outcome that is not a created singleton.
    #[cfg(feature = "lz")]
    pub(crate) fn set_int_metadata(
        &mut self,
        outcome: usize,
        id: i32,
        value: i32,
    ) -> Result<(), NodeError> {
        match self.singles.get_mut(outcome) {
            Some(Some(stream)) => {
                stream.set_metadata(id, value);
                Ok(())
            }
            _ => Err(NodeError::OutcomeOutOfRange),
        }
    }
}

// Appended by the backends package: the format version for `bitunpack`
// (and `dispatch`).
#[cfg(any(feature = "bitunpack", feature = "dispatch"))]
impl NodeCx<'_, '_> {
    /// `ZL_Encoder_getCParam(ZL_CParam_formatVersion)`.
    pub(crate) const fn format_version(&self) -> u32 {
        self.params.version
    }
}

// ---------------------------------------------------------------------
// Helpers added by the structure package (tokenize, split, dispatch, ...).
// ---------------------------------------------------------------------

impl<'a> NodeCx<'_, 'a> {
    /// `ZL_Encoder_createStringStream` with its content already written:
    /// outcome `outcome` becomes the strings of lengths `lens` laid out in
    /// `bytes`.
    ///
    /// Returns the output's index among the node's outputs.
    ///
    /// # Errors
    ///
    /// Lengths that do not sum to the size of `bytes`, or the errors of
    /// [`Self::emit`].
    #[cfg(any(
        feature = "tokenize",
        feature = "concat",
        feature = "interleave",
        feature = "prefix",
        feature = "dispatch"
    ))]
    pub(crate) fn emit_strings(
        &mut self,
        outcome: usize,
        bytes: Vec<u8>,
        lens: Vec<u32>,
    ) -> Result<usize, NodeError> {
        let total: u64 = lens.iter().map(|&len| u64::from(len)).sum();
        if total != bytes.len() as u64 {
            return Err(NodeError::InvalidInput(
                "string lengths do not match the content",
            ));
        }
        self.place(outcome, Stream::strings_from(Bytes::from(bytes), lens))
    }

    /// `ZL_Output_setIntMetadata` on the output with index `output` (as
    /// returned by [`Self::emit`] and its siblings).
    ///
    /// # Errors
    ///
    /// An output that was not created yet.
    #[cfg(any(feature = "split", feature = "dispatch"))]
    pub(crate) fn set_output_metadata(
        &mut self,
        output: usize,
        id: i32,
        value: i32,
    ) -> Result<(), NodeError> {
        let singles = self.singles.len();
        let stream = match output.checked_sub(singles) {
            None => self.singles[output].as_mut(),
            Some(index) => self.variable.get_mut(index).map(|(_, stream)| stream),
        };
        stream
            .ok_or(NodeError::OutcomeOutOfRange)?
            .set_metadata(id, value);
        Ok(())
    }
}
