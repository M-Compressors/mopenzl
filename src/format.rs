//! The OpenZL wire-format version.

use core::fmt;

/// A version of the OpenZL frame format.
///
/// Every frame starts with a magic number naming its format version. This
/// crate writes and reads versions [`Self::MIN`] (21, the first chunked
/// format) to [`Self::MAX`] (27, the newest the reference v0.3.0 writes).
/// The encoder defaults to the newest, as the reference's
/// `ZL_getDefaultEncodingVersion` does; pick an older one only to produce
/// frames for older decoders.
///
/// # Examples
///
/// ```
/// use mopenzl::FormatVersion;
///
/// assert_eq!(FormatVersion::default(), FormatVersion::MAX);
/// assert_eq!(u32::from(FormatVersion::try_from(23)?), 23);
/// assert!(FormatVersion::try_from(20).is_err());
/// assert_eq!(FormatVersion::MIN.to_string(), "21");
/// # Ok::<(), mopenzl::UnsupportedFormatVersion>(())
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FormatVersion(u8);

impl FormatVersion {
    /// The oldest supported version, 21: the first with chunked frames.
    pub const MIN: Self = Self(21);
    /// The newest supported version, 27.
    pub const MAX: Self = Self(27);

    /// The version number.
    pub(crate) const fn get(self) -> u32 {
        self.0 as u32
    }

    /// The version of a supported number.
    pub(crate) const fn new(version: u32) -> Option<Self> {
        if version < Self::MIN.0 as u32 || version > Self::MAX.0 as u32 {
            return None;
        }
        Some(Self(version as u8))
    }
}

impl Default for FormatVersion {
    fn default() -> Self {
        Self::MAX
    }
}

impl From<FormatVersion> for u32 {
    fn from(version: FormatVersion) -> Self {
        version.get()
    }
}

impl TryFrom<u32> for FormatVersion {
    type Error = UnsupportedFormatVersion;

    /// Validates a version number.
    ///
    /// # Errors
    ///
    /// [`UnsupportedFormatVersion`] outside `21..=27`.
    fn try_from(version: u32) -> Result<Self, Self::Error> {
        Self::new(version).ok_or(UnsupportedFormatVersion { version })
    }
}

impl fmt::Display for FormatVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A format version this crate does not support.
///
/// # Examples
///
/// ```
/// use mopenzl::{FormatVersion, UnsupportedFormatVersion};
///
/// assert_eq!(
///     FormatVersion::try_from(8),
///     Err(UnsupportedFormatVersion { version: 8 })
/// );
/// ```
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, thiserror::Error)]
#[error("OpenZL format version {version} is not supported (supported: 21..=27)")]
pub struct UnsupportedFormatVersion {
    /// The rejected version number.
    pub version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn supported_range_is_inclusive() {
        for version in 21..=27 {
            assert_eq!(
                u32::from(FormatVersion::try_from(version).unwrap()),
                version
            );
        }
        for version in [0, 8, 20, 28, u32::MAX] {
            assert_eq!(
                FormatVersion::try_from(version),
                Err(UnsupportedFormatVersion { version })
            );
        }
    }

    #[test]
    fn default_is_the_newest_and_displays_as_a_number() {
        assert_eq!(FormatVersion::default(), FormatVersion::MAX);
        assert_eq!(FormatVersion::MAX.to_string(), "27");
    }

    #[test]
    fn error_names_the_version_and_range() {
        let message = UnsupportedFormatVersion { version: 9 }.to_string();
        assert!(message.contains('9') && message.contains("21..=27"));
    }
}
