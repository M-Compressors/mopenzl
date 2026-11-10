//! The type of a frame's inputs and outputs (`ZL_Type`).

/// What an OpenZL input or output holds.
///
/// Every byte-oriented API reads and writes [`DataType::Serial`] data; the
/// typed APIs carry the others.
///
/// # Examples
///
/// ```
/// use mopenzl::DataType;
///
/// assert_eq!(DataType::default(), DataType::Serial);
/// assert_eq!(DataType::Numeric.to_string(), "numeric");
/// ```
#[derive(Copy, Clone, Debug, Default, Eq, Hash, PartialEq)]
pub enum DataType {
    /// Raw bytes.
    #[default]
    Serial,
    /// Fixed-width records of one width.
    Struct,
    /// Little-endian unsigned integers of 1, 2, 4 or 8 bytes.
    Numeric,
    /// Variable-length strings: concatenated contents plus one length each.
    String,
}

impl core::fmt::Display for DataType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Serial => "serial",
            Self::Struct => "struct",
            Self::Numeric => "numeric",
            Self::String => "string",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_reference() {
        let names = [
            DataType::Serial,
            DataType::Struct,
            DataType::Numeric,
            DataType::String,
        ]
        .map(|ty| alloc::string::ToString::to_string(&ty));
        assert_eq!(names, ["serial", "struct", "numeric", "string"]);
    }
}
