//! Local parameters of nodes and graphs (`ZL_LocalParams`).

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::fmt;

/// A reference parameter: an object a node reads through `&dyn Any`
/// (histograms, Huffman tables, parsers). Compared by identity.
#[derive(Clone)]
pub(crate) struct RefParam(pub(crate) Arc<dyn Any + Send + Sync>);

impl fmt::Debug for RefParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefParam")
    }
}

impl PartialEq for RefParam {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Integer, copy and reference parameters of a node or graph.
///
/// As in the reference, supplying parameters to a node or graph replaces
/// the ones it had instead of merging with them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocalParams {
    /// `(id, value)` integer parameters; the first match of an ID wins.
    pub(crate) ints: Cow<'static, [(i32, i32)]>,
    /// `(id, bytes)` copy parameters.
    pub(crate) copies: Vec<(i32, Arc<[u8]>)>,
    /// `(id, object)` reference parameters.
    pub(crate) refs: Vec<(i32, RefParam)>,
}

impl LocalParams {
    /// No parameters.
    pub(crate) const EMPTY: Self = Self {
        ints: Cow::Borrowed(&[]),
        copies: Vec::new(),
        refs: Vec::new(),
    };

    /// Whether there are no parameters of any kind.
    pub(crate) fn is_empty(&self) -> bool {
        self.ints.is_empty() && self.copies.is_empty() && self.refs.is_empty()
    }

    /// `LP_getLocalIntParam`: the first value for `id`.
    pub(crate) fn int(&self, id: i32) -> Option<i32> {
        self.ints
            .iter()
            .find(|&&(key, _)| key == id)
            .map(|&(_, value)| value)
    }

    /// The copy parameter `id`.
    pub(crate) fn copy(&self, id: i32) -> Option<&[u8]> {
        self.copies
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, bytes)| &**bytes)
    }

    /// The reference parameter `id`.
    pub(crate) fn reference(&self, id: i32) -> Option<&(dyn Any + Send + Sync)> {
        self.refs
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, object)| &*object.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_return_the_first_match() {
        let object: Arc<dyn Any + Send + Sync> = Arc::new(7u32);
        let params = LocalParams {
            ints: Cow::Owned(alloc::vec![(1, 10), (1, 20), (2, 30)]),
            copies: alloc::vec![(5, Arc::from(&b"xy"[..]))],
            refs: alloc::vec![(9, RefParam(Arc::clone(&object)))],
        };
        assert!(!params.is_empty());
        assert!(LocalParams::EMPTY.is_empty());
        assert_eq!(
            (params.int(1), params.int(2), params.int(3)),
            (Some(10), Some(30), None)
        );
        assert_eq!(params.copy(5), Some(&b"xy"[..]));
        assert_eq!(params.copy(6), None);
        let found = params.reference(9).unwrap();
        assert_eq!(found.downcast_ref::<u32>(), Some(&7));
        assert!(params.reference(8).is_none());
        assert_eq!(params.refs[0].1, RefParam(object));
        assert_eq!(alloc::format!("{:?}", params.refs[0].1), "RefParam");
    }
}
