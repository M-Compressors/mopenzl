//! The context of a segmenter (`ZL_Segmenter`, segmenter.c).

use alloc::vec::Vec;

use super::super::data::Stream;
use super::super::error::ExecError;
use super::super::params::LocalParams;
use super::super::registry::{GraphId, RuntimeParams};
use super::Exec;

/// What a segmenter sees: its inputs, how much of them it consumed, and the
/// means to compress the next chunk.
pub(crate) struct SegmenterCx<'s, 'e, 'a> {
    pub(crate) ex: &'s mut Exec<'e, 'a>,
    pub(crate) params: &'s LocalParams,
    pub(crate) custom_graphs: &'s [GraphId],
    inputs: Vec<Stream<'a>>,
    consumed: Vec<usize>,
    chunks: usize,
}

impl<'s, 'e, 'a> SegmenterCx<'s, 'e, 'a> {
    /// `SEGM_init`: takes the full inputs out of the runtime graph.
    pub(crate) fn new(
        ex: &'s mut Exec<'e, 'a>,
        params: &'s LocalParams,
        custom_graphs: &'s [GraphId],
        n_inputs: usize,
    ) -> Result<Self, ExecError> {
        let inputs = (0..n_inputs as u32)
            .map(|n| ex.rt.stream(n).cloned().ok_or(ExecError::GraphInvalid))
            .collect::<Result<Vec<_>, _>>()?;
        ex.rt.reset();
        Ok(Self {
            ex,
            params,
            custom_graphs,
            consumed: alloc::vec![0; n_inputs],
            inputs,
            chunks: 0,
        })
    }

    /// `ZL_Segmenter_numInputs`.
    pub(crate) fn n_inputs(&self) -> usize {
        self.inputs.len()
    }

    /// `ZL_CParam_formatVersion`.
    pub(crate) const fn format_version(&self) -> u32 {
        self.ex.p.version
    }

    /// `ZL_Segmenter_getInput`: input `index` without what was consumed.
    pub(crate) fn input(&self, index: usize) -> Option<Stream<'a>> {
        let input = self.inputs.get(index)?;
        let done = *self.consumed.get(index)?;
        input.slice(done, input.n_elts() - done)
    }

    /// `ZL_Segmenter_getNumElts`: elements left in every input.
    pub(crate) fn remaining(&self) -> Vec<usize> {
        self.inputs
            .iter()
            .zip(&self.consumed)
            .map(|(input, done)| input.n_elts() - done)
            .collect()
    }

    /// `ZL_Segmenter_processChunk`: compresses the next `n_elts[i]` elements
    /// of input `i` as one chunk through `start`.
    ///
    /// # Errors
    ///
    /// A wrong number of inputs, more elements than are left, or any failure
    /// of the chunk's graph.
    pub(crate) fn process_chunk(
        &mut self,
        n_elts: &[usize],
        start: GraphId,
        params: Option<&RuntimeParams>,
    ) -> Result<(), ExecError> {
        if n_elts.len() != self.inputs.len() {
            return Err(ExecError::SuccessorInputs);
        }
        let mut chunk = Vec::with_capacity(n_elts.len());
        for (index, &count) in n_elts.iter().enumerate() {
            let slice = self.inputs[index]
                .slice(self.consumed[index], count)
                .ok_or(ExecError::Parameter("chunk larger than the input"))?;
            chunk.push(slice);
        }
        for (done, &count) in self.consumed.iter_mut().zip(n_elts) {
            *done += count;
        }
        self.ex.rt.reset();
        let rtsids: Vec<u32> = chunk
            .iter()
            .map(|stream| self.ex.rt.ref_input(stream.clone()))
            .collect();
        let depth = self.ex.segmenter_depth + 1;
        self.ex.run_successor(start, params, &rtsids, depth)?;
        self.ex.flush_chunk(&chunk)?;
        self.chunks += 1;
        Ok(())
    }

    /// `SEGM_runSegmenter`'s postconditions.
    pub(crate) fn check_done(&self) -> Result<(), ExecError> {
        if self.chunks == 0 {
            return Err(ExecError::SegmenterNoChunks);
        }
        if self
            .inputs
            .iter()
            .zip(&self.consumed)
            .any(|(input, &done)| done < input.n_elts())
        {
            return Err(ExecError::SegmenterNotConsumed);
        }
        Ok(())
    }
}
