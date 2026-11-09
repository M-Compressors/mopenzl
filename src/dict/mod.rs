//! Dictionary bundles (format version 25 and newer).
//!
//! A codec may need side information the frame does not carry, such as a
//! trained Zstandard dictionary. Dictionaries travel in a [`DictBundle`]:
//! a list of [`Dict`]s identified by the SHA-256 of their content. A frame
//! names the bundle it was compressed with ([`BundleId`], the SHA-256 of the
//! dictionary IDs) and each codec of a chunk names its dictionary by index
//! in the bundle.
//!
//! On the encoder side, [`GraphBuilder::load_dictionaries`] attaches a
//! bundle to a compressor graph and a node references a dictionary with
//! [`NodeParams::with_dictionary`]. On the decoder side a [`BundleStore`]
//! holds the bundles frames may refer to; give it to
//! [`DecompressorBuilder::with_dictionaries`].
//!
//! The serialized *fat bundle* (info plus the packed dictionaries) is
//! OpenZL's interchange format: [`DictBundle`] parses and packs it, and the
//! reference's training tools produce it.
//!
//! # Examples
//!
//! ```
//! # #[cfg(all(feature = "compression", feature = "decompression", feature = "zstd"))]
//! # {
//! use mopenzl::dict::{BundleStore, Dict, DictBundle};
//! use mopenzl::graph::{GraphBuilder, NodeParams, StandardGraph, StandardNode};
//! use mopenzl::{Compressor, Decompressor, EncoderConfig};
//!
//! // A raw-content Zstandard dictionary, prepared for level 3.
//! let history = b"{\"user\":\"alice\",\"city\":\"Oslo\",\"plan\":\"pro\"}".repeat(4);
//! let dict = Dict::zstd(&history, 3)?;
//! let bundle = DictBundle::try_from(vec![dict.clone()])?;
//!
//! let mut builder = GraphBuilder::new();
//! let node = builder.parameterize_node_with(
//!     StandardNode::Zstd,
//!     NodeParams::default().with_dictionary(dict.id()),
//! )?;
//! builder.load_dictionaries(&bundle)?;
//! let start = builder.static_graph(node, &[StandardGraph::Store.into()])?;
//! let graph = builder.build(start)?;
//!
//! let mut compressor = Compressor::builder(EncoderConfig::default()).with_graph(graph).build()?;
//! let record = b"{\"user\":\"alice\",\"city\":\"Oslo\",\"plan\":\"free\"}";
//! let frame = compressor.compress(record)?;
//!
//! let mut store = BundleStore::default();
//! store.insert(&bundle)?;
//! let mut decoder = Decompressor::builder(Default::default()).with_dictionaries(store).build();
//! assert_eq!(decoder.decompress(&frame)?, record);
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
#![cfg_attr(
    feature = "compression",
    doc = "[`GraphBuilder::load_dictionaries`]: crate::graph::GraphBuilder::load_dictionaries\n\n[`NodeParams::with_dictionary`]: crate::graph::NodeParams::with_dictionary"
)]
#![cfg_attr(
    not(feature = "compression"),
    doc = "[`GraphBuilder::load_dictionaries`]: crate#select-codecs\n\n[`NodeParams::with_dictionary`]: crate#select-codecs"
)]
#![cfg_attr(
    feature = "decompression",
    doc = "[`DecompressorBuilder::with_dictionaries`]: crate::DecompressorBuilder::with_dictionaries"
)]
#![cfg_attr(
    not(feature = "decompression"),
    doc = "[`BundleStore`]: crate#select-codecs\n\n[`DecompressorBuilder::with_dictionaries`]: crate#select-codecs"
)]

pub(crate) mod core;

use ::core::fmt;
use alloc::string::String;
use alloc::vec::Vec;

use thiserror::Error;

use crate::shared::dictwire::{self, ID_SIZE};
use crate::shared::sha256;

macro_rules! unique_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name([u8; ID_SIZE]);

        impl From<[u8; ID_SIZE]> for $name {
            fn from(bytes: [u8; ID_SIZE]) -> Self {
                Self(bytes)
            }
        }

        impl From<$name> for [u8; ID_SIZE] {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl AsRef<[u8]> for $name {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl fmt::Display for $name {
            /// Lowercase hexadecimal, 64 digits.
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self)
            }
        }
    };
}

unique_id! {
    /// The identifier of a [`Dict`]: 32 bytes, by default the SHA-256 of its
    /// content (`ZL_DictID`).
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::dict::DictId;
    ///
    /// let id = DictId::from([0xab; 32]);
    /// assert_eq!(id.to_string(), "ab".repeat(32));
    /// assert_eq!(<[u8; 32]>::from(id), [0xab; 32]);
    /// ```
    DictId
}

unique_id! {
    /// The identifier of a [`DictBundle`]: the SHA-256 of its dictionary IDs
    /// in order, recorded in the header of every frame compressed with the
    /// bundle (`ZL_BundleID`).
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::dict::BundleId;
    ///
    /// assert_eq!(BundleId::from([1; 32]).as_ref().len(), 32);
    /// ```
    BundleId
}

/// Why a dictionary or bundle could not be built, parsed or loaded.
///
/// # Examples
///
/// ```
/// use mopenzl::dict::{DictBundle, DictError};
///
/// assert!(matches!(DictBundle::try_from(&b"not a bundle"[..]), Err(DictError::Malformed(_))));
/// ```
#[derive(Error, Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum DictError {
    /// Bytes that are not a dictionary or a fat bundle: a wrong magic number,
    /// a truncated structure, or an ID that differs from the one the bundle
    /// declares. The message says which.
    #[error("malformed dictionary data: {0}")]
    Malformed(String),
    /// More dictionaries than a bundle holds (65 535).
    #[error("{count} dictionaries exceed the limit of 65535 per bundle")]
    TooManyDicts {
        /// The number of dictionaries.
        count: usize,
    },
    /// A dictionary content of 4 GiB or more.
    #[error("a dictionary content is limited to 4 GiB - 1")]
    ContentTooLarge,
    /// Two dictionaries share an ID but not a content.
    #[error("two different dictionary contents share the ID {0}")]
    ConflictingId(DictId),
    /// No materializer exists for the codec of a dictionary: only standard
    /// Zstandard dictionaries can be loaded, and only with the `zstd`
    /// feature.
    #[error("no materializer for dictionaries of codec {}{}", .codec.id(), if .codec.is_custom() { " (custom)" } else { "" })]
    NoMaterializer {
        /// The codec the dictionary names.
        codec: DictCodec,
    },
    /// The codec rejected the dictionary's content.
    #[error("dictionary materialization failed: {0}")]
    Materialization(String),
    /// A [`BundleStore`] holds 128 bundles, or 65 535 dictionaries, at most.
    ///
    #[cfg_attr(
        not(feature = "decompression"),
        doc = "[`BundleStore`]: crate#select-codecs"
    )]
    #[error("the bundle store is full")]
    StoreFull,
}

/// The codec that materializes a dictionary (`materializingCodec` and the
/// custom flag of a packed dictionary).
///
/// # Examples
///
/// ```
/// use mopenzl::dict::DictCodec;
///
/// let custom = DictCodec::from((7, true));
/// assert_eq!((custom.id(), custom.is_custom()), (7, true));
/// assert_ne!(custom, DictCodec::ZSTD);
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct DictCodec {
    id: u32,
    custom: bool,
}

impl DictCodec {
    /// The standard Zstandard codec (`ZL_StandardTransformID_zstd`), the
    /// only one dictionaries can be materialized for.
    pub const ZSTD: Self = Self {
        id: 22,
        custom: false,
    };

    /// The codec ID.
    #[must_use]
    pub const fn id(self) -> u32 {
        self.id
    }

    /// Whether the codec was registered by the application.
    #[must_use]
    pub const fn is_custom(self) -> bool {
        self.custom
    }
}

impl From<(u32, bool)> for DictCodec {
    /// `(codec ID, custom)`.
    fn from((id, custom): (u32, bool)) -> Self {
        Self { id, custom }
    }
}

/// A dictionary: opaque content for one codec, with an ID.
///
/// # Examples
///
/// ```
/// use mopenzl::dict::{Dict, DictCodec};
///
/// let dict = Dict::new(DictCodec::ZSTD, b"content".to_vec())?;
/// let packed = Vec::from(&dict);
/// assert_eq!(Dict::try_from(&packed[..])?, dict);
/// # Ok::<(), mopenzl::dict::DictError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dict {
    id: DictId,
    codec: DictCodec,
    content: Vec<u8>,
}

impl Dict {
    /// A dictionary of `content` for `codec`, identified by the SHA-256 of
    /// the content.
    ///
    /// # Errors
    ///
    /// [`DictError::ContentTooLarge`] for 4 GiB or more.
    pub fn new(codec: DictCodec, content: impl Into<Vec<u8>>) -> Result<Self, DictError> {
        let content = content.into();
        u32::try_from(content.len()).map_err(|_| DictError::ContentTooLarge)?;
        Ok(Self {
            id: DictId(sha256::digest(&content)),
            codec,
            content,
        })
    }

    /// A Zstandard dictionary: `raw` in Zstandard's dictionary format (what
    /// `zstd --train` writes) or raw content, prepared for compression
    /// `level` (`ZL_TrainedZstdContent_pack`).
    ///
    /// # Errors
    ///
    /// [`DictError::ContentTooLarge`] for 4 GiB or more.
    ///
    /// # Examples
    ///
    /// ```
    /// use mopenzl::dict::{Dict, DictCodec};
    ///
    /// let dict = Dict::zstd(b"raw dictionary", 3)?;
    /// assert_eq!(dict.codec(), DictCodec::ZSTD);
    /// assert_eq!(dict.content().len(), 8 + 14);
    /// # Ok::<(), mopenzl::dict::DictError>(())
    /// ```
    pub fn zstd(raw: &[u8], level: i32) -> Result<Self, DictError> {
        Self::new(DictCodec::ZSTD, dictwire::pack_zstd_content(level, raw))
    }

    /// Replaces the ID (the default is the SHA-256 of the content). An
    /// all-zero ID means "unset" in the format and restores the default.
    #[must_use]
    pub fn with_id(mut self, id: DictId) -> Self {
        self.id = if id.0 == [0; ID_SIZE] {
            DictId(sha256::digest(&self.content))
        } else {
            id
        };
        self
    }

    /// The dictionary's ID.
    #[must_use]
    pub const fn id(&self) -> DictId {
        self.id
    }

    /// The codec that materializes it.
    #[must_use]
    pub const fn codec(&self) -> DictCodec {
        self.codec
    }

    /// The content, in the codec's own serialization.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

impl From<&Dict> for Vec<u8> {
    /// The packed dictionary (`Dict_pack`).
    fn from(dict: &Dict) -> Self {
        let mut out = Vec::new();
        // Packing fails only on a content of 4 GiB or more, which `Dict`
        // never holds.
        let _ = dictwire::pack_dict(
            &mut out,
            &dict.id.0,
            dict.codec.id,
            dict.codec.custom,
            &dict.content,
        );
        out
    }
}

impl TryFrom<&[u8]> for Dict {
    type Error = DictError;

    /// Parses a packed dictionary (`ZL_Dict_parse`); bytes after it are
    /// ignored.
    ///
    /// # Errors
    ///
    /// [`DictError::Malformed`] when the bytes are not a packed dictionary.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let parsed = dictwire::parse_dict(bytes)?;
        Ok(Self {
            id: DictId(parsed.id),
            codec: DictCodec::from((parsed.codec, parsed.custom)),
            content: parsed.content.to_vec(),
        })
    }
}

/// An ordered list of dictionaries with the ID frames record.
///
/// Codecs address a dictionary by its position, so the order is part of the
/// bundle's identity.
///
/// # Examples
///
/// ```
/// use mopenzl::dict::{Dict, DictBundle, DictCodec};
///
/// let dicts = vec![
///     Dict::new(DictCodec::ZSTD, b"one".to_vec())?,
///     Dict::new(DictCodec::ZSTD, b"two".to_vec())?,
/// ];
/// let bundle = DictBundle::try_from(dicts)?;
/// let fat = Vec::from(&bundle);
/// assert_eq!(DictBundle::try_from(&fat[..])?, bundle);
/// assert_eq!(bundle.dicts().len(), 2);
/// # Ok::<(), mopenzl::dict::DictError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DictBundle {
    id: BundleId,
    dicts: Vec<Dict>,
}

impl DictBundle {
    /// The bundle's ID.
    #[must_use]
    pub const fn id(&self) -> BundleId {
        self.id
    }

    /// The dictionaries in order.
    #[must_use]
    pub fn dicts(&self) -> &[Dict] {
        &self.dicts
    }
}

impl TryFrom<Vec<Dict>> for DictBundle {
    type Error = DictError;

    /// A bundle of `dicts`, whose ID is the SHA-256 of their IDs.
    ///
    /// # Errors
    ///
    /// [`DictError::TooManyDicts`] above 65 535 dictionaries.
    fn try_from(dicts: Vec<Dict>) -> Result<Self, Self::Error> {
        if dicts.len() > dictwire::MAX_DICTS_PER_BUNDLE {
            return Err(DictError::TooManyDicts { count: dicts.len() });
        }
        let id = BundleId(dictwire::bundle_id(dicts.iter().map(|dict| &dict.id.0)));
        Ok(Self { id, dicts })
    }
}

impl TryFrom<&[u8]> for DictBundle {
    type Error = DictError;

    /// Parses a fat bundle: the bundle info, then the packed dictionaries
    /// it lists, each checked against its declared ID. Like the reference,
    /// this trusts the stored bundle ID and ignores bytes after the last
    /// dictionary.
    ///
    /// # Errors
    ///
    /// [`DictError::Malformed`] for a truncated or inconsistent bundle, a
    /// bundle that is not fat, or a wrong magic number;
    /// [`DictError::TooManyDicts`] for a count above 65 535.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let parsed = dictwire::parse_fat_bundle(bytes)?;
        let dicts = parsed
            .dicts
            .iter()
            .map(|dict| Dict {
                id: DictId(dict.id),
                codec: DictCodec::from((dict.codec, dict.custom)),
                content: dict.content.to_vec(),
            })
            .collect();
        Ok(Self {
            id: BundleId(parsed.id),
            dicts,
        })
    }
}

impl From<&DictBundle> for Vec<u8> {
    /// The fat bundle (`ZL_DictBundle_packFatBundle`).
    fn from(bundle: &DictBundle) -> Self {
        let packed: Vec<Vec<u8>> = bundle.dicts.iter().map(Vec::from).collect();
        let parsed: Vec<_> = packed
            .iter()
            .filter_map(|bytes| dictwire::parse_dict(bytes).ok())
            .collect();
        // Packing fails only past 65 535 dictionaries, which a bundle never
        // holds.
        dictwire::pack_fat_bundle(&parsed).unwrap_or_default()
    }
}

#[cfg(feature = "decompression")]
mod store;
#[cfg(feature = "decompression")]
pub use store::BundleStore;
#[cfg(feature = "decompression")]
pub(crate) use store::StoredBundle;

#[cfg(test)]
mod tests;
