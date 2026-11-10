//! The per-chunk runtime graph (`RTGraph`, rtgraphs.c).
//!
//! Nodes and streams are numbered in creation order. A node owns the
//! contiguous block of streams it created: its declared singleton outputs
//! first (their slots are reserved when the node is created), then its
//! variable outputs in the order it produced them. The chunk header is
//! derived from nothing but this numbering, so it is reproduced exactly.

use alloc::vec::Vec;

use super::data::Stream;

/// A node that ran in this chunk.
#[derive(Clone, Debug)]
pub(crate) struct RtNode {
    /// The wire codec ID.
    pub(crate) codec: u32,
    pub(crate) standard: bool,
    /// Input stream indices, in port order.
    pub(crate) inputs: Vec<u32>,
    /// Index of the node's first output stream.
    pub(crate) start_out: u32,
    /// Outputs the node created.
    pub(crate) n_out: u32,
    /// Declared singleton outputs.
    pub(crate) n_singletons: u32,
    /// The private header: `(start, len)` in the staging buffer.
    pub(crate) header: (usize, usize),
    /// Index of the node's dictionary in the bundle.
    pub(crate) dict_index: Option<u32>,
}

/// A stream of the chunk.
#[derive(Debug)]
pub(crate) struct RtStream<'a> {
    /// The content; `None` for a reserved slot not yet filled.
    pub(crate) stream: Option<Stream<'a>>,
    /// The node outcome that produced it.
    pub(crate) outcome: u32,
    /// Whether the stream is written to the frame.
    pub(crate) to_store: bool,
}

/// Why the runtime graph refused a change.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum RtError {
    /// More nodes or streams than the format version allows.
    Limit,
}

/// Nodes, streams and staged codec headers of the chunk in progress.
#[derive(Debug, Default)]
pub(crate) struct RtGraph<'a> {
    pub(crate) nodes: Vec<RtNode>,
    pub(crate) streams: Vec<RtStream<'a>>,
    /// Codec headers in the order nodes sent them (`stagingHeaderStream`).
    pub(crate) staging: Vec<u8>,
}

impl<'a> RtGraph<'a> {
    /// `RTGM_reset`.
    pub(crate) fn reset(&mut self) {
        self.nodes.clear();
        self.streams.clear();
        self.staging.clear();
    }

    /// `RTGM_refInput`: adds an input stream.
    pub(crate) fn ref_input(&mut self, stream: Stream<'a>) -> u32 {
        self.streams.push(RtStream {
            stream: Some(stream),
            outcome: 0,
            to_store: false,
        });
        (self.streams.len() - 1) as u32
    }

    /// `RTGM_createNode`: registers a node and reserves its singleton slots.
    pub(crate) fn create_node(
        &mut self,
        codec: u32,
        n_singletons: usize,
        inputs: &[u32],
        dict_index: Option<u32>,
        node_limit: usize,
        stream_limit: usize,
    ) -> Result<usize, RtError> {
        if self.nodes.len() >= node_limit || self.streams.len() + n_singletons > stream_limit {
            return Err(RtError::Limit);
        }
        self.nodes.push(RtNode {
            standard: true,
            codec,
            inputs: inputs.to_vec(),
            start_out: self.streams.len() as u32,
            n_out: 0,
            n_singletons: n_singletons as u32,
            header: (0, 0),
            dict_index,
        });
        for _ in 0..n_singletons {
            self.streams.push(RtStream {
                stream: None,
                outcome: 0,
                to_store: false,
            });
        }
        Ok(self.nodes.len() - 1)
    }

    /// Fills a singleton slot of `node`.
    pub(crate) fn fill_singleton(&mut self, node: usize, outcome: usize, stream: Stream<'a>) {
        let slot = self.nodes[node].start_out as usize + outcome;
        self.streams[slot] = RtStream {
            stream: Some(stream),
            outcome: outcome as u32,
            to_store: false,
        };
        self.nodes[node].n_out += 1;
    }

    /// Appends a variable output of `node` (`RTGM_reserveOutputStreamSlot`).
    pub(crate) fn push_variable(
        &mut self,
        node: usize,
        outcome: usize,
        stream: Stream<'a>,
        stream_limit: usize,
    ) -> Result<(), RtError> {
        if self.streams.len() >= stream_limit {
            return Err(RtError::Limit);
        }
        self.streams.push(RtStream {
            stream: Some(stream),
            outcome: outcome as u32,
            to_store: false,
        });
        self.nodes[node].n_out += 1;
        Ok(())
    }

    /// Stages the codec header of `node` (`CCTX_sendTrHeader`).
    pub(crate) fn set_header(&mut self, node: usize, header: &[u8]) {
        self.nodes[node].header = (self.staging.len(), header.len());
        self.staging.extend_from_slice(header);
    }

    /// `RTGM_storeStream`.
    pub(crate) fn store(&mut self, stream: u32) {
        self.streams[stream as usize].to_store = true;
    }

    /// The stream `index`, if filled.
    pub(crate) fn stream(&self, index: u32) -> Option<&Stream<'a>> {
        self.streams.get(index as usize)?.stream.as_ref()
    }

    /// `RTGM_clearNodesFrom`: drops the nodes from `rank` on and every
    /// stream they created. Like the reference, it leaves the store marks of
    /// earlier streams alone.
    pub(crate) fn clear_nodes_from(&mut self, rank: usize) {
        if rank >= self.nodes.len() {
            return;
        }
        let first_stream = self.nodes[rank].start_out as usize;
        self.streams.truncate(first_stream);
        self.nodes.truncate(rank);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compressor::core::data::{Bytes, Type};

    fn serial(len: usize) -> Stream<'static> {
        Stream::new(Type::Serial, 1, Bytes::from(alloc::vec![0u8; len]))
    }

    #[test]
    fn singleton_slots_precede_variable_outputs() {
        let mut graph = RtGraph::default();
        let input = graph.ref_input(serial(4));
        let node = graph.create_node(9, 2, &[input], None, 10, 10).unwrap();
        graph.push_variable(node, 2, serial(1), 10).unwrap();
        graph.fill_singleton(node, 1, serial(2));
        graph.fill_singleton(node, 0, serial(3));
        assert_eq!(graph.nodes[node].start_out, 1);
        assert_eq!(graph.nodes[node].n_out, 3);
        let sizes: Vec<_> = (1..4)
            .map(|i| graph.stream(i).unwrap().content_size())
            .collect();
        assert_eq!(sizes, [3, 2, 1]);
        assert_eq!(graph.streams[3].outcome, 2);
    }

    #[test]
    fn limits_headers_store_marks_and_clearing() {
        let mut graph = RtGraph::default();
        let input = graph.ref_input(serial(4));
        assert_eq!(
            graph.create_node(1, 1, &[input], None, 0, 10),
            Err(RtError::Limit)
        );
        assert_eq!(
            graph.create_node(1, 20, &[input], None, 5, 10),
            Err(RtError::Limit)
        );
        let first = graph.create_node(1, 1, &[input], Some(3), 5, 10).unwrap();
        graph.fill_singleton(first, 0, serial(1));
        graph.set_header(first, b"ab");
        let second = graph.create_node(2, 0, &[1], None, 5, 10).unwrap();
        graph.set_header(second, b"c");
        assert_eq!(graph.nodes[first].header, (0, 2));
        assert_eq!(graph.nodes[second].header, (2, 1));
        assert_eq!(graph.nodes[first].dict_index, Some(3));
        assert_eq!(
            graph.push_variable(second, 0, serial(1), 2),
            Err(RtError::Limit)
        );
        graph.store(0);
        assert!(graph.streams[0].to_store);
        graph.clear_nodes_from(7);
        assert_eq!(graph.nodes.len(), 2);
        graph.clear_nodes_from(1);
        assert_eq!((graph.nodes.len(), graph.streams.len()), (1, 2));
        assert!(graph.streams[0].to_store);
        assert!(graph.stream(9).is_none());
        graph.reset();
        assert!(graph.nodes.is_empty() && graph.streams.is_empty());
    }
}
