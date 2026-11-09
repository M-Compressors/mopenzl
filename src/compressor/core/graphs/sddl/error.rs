//! The private error of the SDDL graph, folded into
//! [`ExecError`](crate::compressor::core::error::ExecError) at the graph
//! boundary.

use thiserror::Error;

use crate::compressor::core::error::ExecError;
use crate::shared::cbor::Error as CborError;

/// Why a description could not be loaded or run.
#[derive(Error, Debug, Clone, Eq, PartialEq)]
pub(super) enum SddlError {
    /// The description is not CBOR the reference accepts.
    #[error("invalid SDDL description encoding")]
    Cbor(#[from] CborError),
    /// The description or the data it parses is inconsistent
    /// (`ZL_E_corruption`).
    #[error("{0}")]
    Corruption(&'static str),
    /// A limit of the interpreter (`allocation` errors in the reference).
    #[error("{0}")]
    Limit(&'static str),
    /// The description fails on purpose or on this data
    /// (`ZL_E_GENERIC`).
    #[error("{0}")]
    Generic(&'static str),
    /// The data is shorter than the description needs.
    #[error("the description reads past the end of the input")]
    SrcTooSmall,
    /// The description left input unparsed.
    #[error("the data description did not consume the whole input")]
    SrcTooLarge,
}

impl From<SddlError> for ExecError {
    fn from(error: SddlError) -> Self {
        ExecError::Parameter(match error {
            SddlError::Cbor(_) => "invalid SDDL description encoding",
            SddlError::Corruption(message)
            | SddlError::Limit(message)
            | SddlError::Generic(message) => message,
            SddlError::SrcTooSmall => "the SDDL description reads past the end of the input",
            SddlError::SrcTooLarge => "the SDDL description did not consume the whole input",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::cbor::item::ErrorKind;
    use alloc::string::ToString;
    use core::error::Error;

    #[test]
    fn errors_convert_to_graph_errors_and_keep_their_source() {
        let cbor = SddlError::from(CborError {
            kind: ErrorKind::Truncated,
            position: 0,
            depth: 0,
        });
        assert!(cbor.source().is_some());
        for error in [
            cbor,
            SddlError::Corruption("c"),
            SddlError::Limit("l"),
            SddlError::Generic("g"),
            SddlError::SrcTooSmall,
            SddlError::SrcTooLarge,
        ] {
            assert!(!error.to_string().is_empty());
            assert!(matches!(ExecError::from(error), ExecError::Parameter(_)));
        }
    }
}
