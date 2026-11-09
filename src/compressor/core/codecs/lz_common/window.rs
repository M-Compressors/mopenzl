//! The match window of `common/window.{h,c}`, reduced to indices.
//!
//! The reference tracks pointers so that blocks can arrive in separate
//! buffers. The `field_lz` codec feeds it one buffer, so only the index
//! bookkeeping of a single segment is needed: the first byte of the
//! segment has index 1 (index 0 marks an empty table slot) and nothing
//! below it is valid.

/// `ZS_window` for a single segment.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Window {
    next_index: u32,
    dict_limit: u32,
    low_limit: u32,
    min_dict_size: u32,
}

impl Window {
    /// `ZS_window_init` (the maximum distance is not used by the match
    /// finders and is not kept).
    pub(crate) const fn new(min_dict_size: u32) -> Self {
        Self {
            next_index: 1,
            dict_limit: 1,
            low_limit: 1,
            min_dict_size,
        }
    }

    /// `ZS_window_update` for a segment that does not continue the
    /// previous one: the old prefix becomes the dictionary unless it is
    /// smaller than the minimum dictionary size.
    pub(crate) fn update(&mut self, size: u32) {
        self.low_limit = self.dict_limit;
        self.dict_limit = self.next_index;
        if self.dict_limit - self.low_limit < self.min_dict_size {
            self.low_limit = self.dict_limit;
        }
        self.next_index += size;
    }

    /// The lowest valid index.
    pub(crate) const fn low_limit(&self) -> u32 {
        self.low_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_segment_starts_at_index_one_with_nothing_before_it() {
        let mut window = Window::new(8);
        assert_eq!(window.low_limit(), 1);
        window.update(100);
        assert_eq!(window.low_limit(), 1);
    }

    #[test]
    fn a_second_segment_keeps_a_large_enough_previous_one_as_dictionary() {
        let mut window = Window::new(8);
        window.update(100);
        window.update(50);
        assert_eq!(window.low_limit(), 1);
        let mut small = Window::new(8);
        small.update(4);
        small.update(50);
        assert_eq!(small.low_limit(), 5);
    }
}
