//! Private errors of the graph engine. They map one to one onto the
//! reference error codes that change control flow (permissive fallbacks,
//! trial compressions), and are folded into the public
//! [`EncodeError`](crate::EncodeError) at the API boundary.

use thiserror::Error;

use super::sink::Full;

/// Why a node could not run.
#[derive(Error, Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum NodeError {
    #[cfg(feature = "custom-codec")]
    #[error("custom codec failed: {0}")]
    Custom(#[source] crate::custom_codec::CodecError),
    /// The codec's Cargo feature is disabled.
    #[error("the {0} codec is disabled by its Cargo feature")]
    Disabled(&'static str),
    /// A deprecated node the encoder no longer writes.
    #[error("the {0} node is deprecated")]
    Legacy(&'static str),
    /// An input the codec cannot take.
    #[error("{0}")]
    InvalidInput(&'static str),
    /// A local parameter missing or out of range.
    #[error("{0}")]
    InvalidParam(&'static str),
    /// A parameter this port does not support yet.
    #[error("unsupported parameter: {0}")]
    UnsupportedParam(&'static str),
    /// An output outcome outside the node's declaration.
    #[error("output outcome out of range")]
    OutcomeOutOfRange,
    /// An output created twice.
    #[error("output created twice")]
    OutputTwice,
    /// A declared singleton output the node never created.
    #[error("a singleton output was not committed")]
    OutputNotCommitted,
    /// A codec header sent twice or beyond the limit.
    #[error("invalid codec header")]
    Header,
    /// The backend (Zstandard) failed.
    #[cfg(feature = "zstd")]
    #[error("{0} backend failure")]
    Backend(&'static str),
}

/// Why a graph, a node run or a frame failed.
#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub(crate) enum ExecError {
    /// A node failed.
    #[error("node {node} failed: {source}")]
    Node {
        /// The node name.
        node: &'static str,
        /// Why.
        #[source]
        source: NodeError,
    },
    /// The wrong number of inputs for a node.
    #[error("node {0} received an unexpected number of inputs")]
    NodeInputCount(&'static str),
    /// An input of the wrong type for a node.
    #[error("node {0} received an input of the wrong type")]
    NodeInputType(&'static str),
    /// The node does not exist at the format version.
    #[error("node {0} is not available at this format version")]
    NodeVersion(&'static str),
    /// A graph ID that does not exist or does not fit the call.
    #[error("invalid graph")]
    GraphInvalid,
    /// A node ID that does not exist.
    #[error("invalid node")]
    NodeInvalid,
    /// A successor given the wrong number of inputs.
    #[error("a successor received an unexpected number of inputs")]
    SuccessorInputs,
    /// An edge given a destination twice or after it was consumed.
    #[error("an edge already has a successor")]
    SuccessorAlreadySet,
    /// An edge no graph consumed.
    #[error("an edge has no successor")]
    SuccessorMissing,
    /// A runaway graph recursion.
    #[error("graph depth limit exceeded")]
    DepthExceeded,
    /// A segmenter used anywhere but on the frame's full input.
    #[error("a segmenter can only run on the full input")]
    SegmenterPlacement,
    /// A segmenter given inputs it would need to convert.
    #[error("segmenters do not convert their inputs")]
    SegmenterConversion,
    /// A segmenter that produced no chunk.
    #[error("a segmenter must produce at least one chunk")]
    SegmenterNoChunks,
    /// A segmenter that left input unconsumed.
    #[error("a segmenter must consume its whole input")]
    SegmenterNotConsumed,
    /// A graph parameter missing or out of range.
    #[error("{0}")]
    Parameter(&'static str),
    /// No implicit conversion between two stream types.
    #[error("no implicit conversion between the stream types")]
    NoConversion,
    /// A limit of the format version.
    #[error("{0} exceeds the format limit")]
    Limit(&'static str),
    /// The destination is full.
    #[error("the destination is full")]
    Full,
    /// A standard graph whose codec's Cargo feature is off.
    #[cfg(any(
        test,
        not(all(
            feature = "field-lz",
            feature = "lz",
            feature = "entropy",
            feature = "partition",
            feature = "merge-sorted"
        ))
    ))]
    #[error("the graph's codec is disabled by its Cargo feature")]
    Unported,
    /// A user graph, selector or segmenter reported a failure.
    #[error("{0}")]
    User(&'static str),
}

impl From<Full> for ExecError {
    fn from(_: Full) -> Self {
        Self::Full
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use core::error::Error;

    #[test]
    fn messages_and_sources_describe_the_failure() {
        let error = ExecError::Node {
            node: "zstd",
            source: NodeError::Disabled("zstd"),
        };
        assert!(error.to_string().contains("zstd failed"));
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("the zstd codec is disabled by its Cargo feature")
        );
        assert_eq!(ExecError::from(Full), ExecError::Full);
        let messages = [
            NodeError::Disabled("lz").to_string(),
            NodeError::Legacy("rolz").to_string(),
            NodeError::UnsupportedParam("x").to_string(),
            NodeError::OutcomeOutOfRange.to_string(),
            NodeError::OutputTwice.to_string(),
            NodeError::OutputNotCommitted.to_string(),
            NodeError::Header.to_string(),
            #[cfg(feature = "zstd")]
            NodeError::Backend("zstd").to_string(),
            NodeError::InvalidInput("in").to_string(),
            NodeError::InvalidParam("p").to_string(),
        ];
        assert!(messages.iter().all(|message| !message.is_empty()));
        let errors = [
            ExecError::NodeInputCount("n"),
            ExecError::NodeInputType("n"),
            ExecError::NodeVersion("n"),
            ExecError::GraphInvalid,
            ExecError::NodeInvalid,
            ExecError::SuccessorInputs,
            ExecError::SuccessorAlreadySet,
            ExecError::SuccessorMissing,
            ExecError::DepthExceeded,
            ExecError::SegmenterPlacement,
            ExecError::SegmenterConversion,
            ExecError::SegmenterNoChunks,
            ExecError::SegmenterNotConsumed,
            ExecError::Parameter("p"),
            ExecError::NoConversion,
            ExecError::Limit("nodes"),
            ExecError::Full,
            ExecError::User("u"),
            ExecError::Unported,
        ];
        assert!(errors.iter().all(|error| !error.to_string().is_empty()));
    }
}
