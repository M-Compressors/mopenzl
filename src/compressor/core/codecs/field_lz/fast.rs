//! The fast match finder of `field_lz` (`ZS_tokenLzMatchFinder`,
//! `encode_match_finder_fast_field_lz.c`), used at levels 1 to 3.
//!
//! One or two hash tables (a short and, when `large`, a long hash of the
//! field-aligned source) give one candidate each; two repcodes are tried
//! first. Matches are extended forward and backward in whole fields. A ring
//! of precomputed hashes ([`HashCache`]) replaces the reference's prefetch
//! pipeline; it is ported exactly because its stale slots decide which
//! candidates are looked up.
//!
//! Positions are offsets into the source. The tables hold *indices*, which
//! are the offset plus one (the window's base pointer is one byte before the
//! source, so that index 0 means "empty").

use super::{SeqStore, Sequence, check_match, count_fields};
use crate::compressor::core::codecs::lz_common::fast_table::FastTable;

/// `kMaxOffset`.
const MAX_OFFSET: u32 = (1 << 24) - 1;
/// `kHashSize`: the slots of the hash cache.
const HASH_SIZE: usize = 16;
/// `kHashMask`.
const HASH_MASK: usize = HASH_SIZE - 1;

/// `smallMatchLength`.
const fn small_match_length(field_size: u32) -> u32 {
    if field_size == 4 {
        8
    } else if field_size < 4 {
        4
    } else {
        field_size
    }
}

/// `largeMatchLength`.
const fn large_match_length(field_size: u32) -> u32 {
    if field_size == 4 {
        12
    } else if field_size < 4 {
        8
    } else {
        2 * field_size
    }
}

/// `ZS_HashCache`: the table hashes of the next sixteen fields.
#[derive(Default)]
struct HashCache {
    small: [u32; HASH_SIZE],
    large: [u32; HASH_SIZE],
    /// The position after the last field `update` handed out; `None` after
    /// a re-initialisation (the reference zeroes the pointer).
    next: Option<isize>,
}

/// The state of one parse.
struct Finder<'a> {
    src: &'a [u8],
    field_size: u32,
    field_bits: u32,
    k_small: u32,
    k_large: u32,
    small: FastTable,
    large: Option<FastTable>,
    cache: HashCache,
    /// The lowest valid table index (`window->lowLimit`).
    low_limit: u32,
}

impl<'a> Finder<'a> {
    fn new(
        src: &'a [u8],
        field_size: u32,
        table_log: u32,
        use_large: bool,
        low_limit: u32,
    ) -> Self {
        let k_small = small_match_length(field_size);
        let k_large = large_match_length(field_size);
        Self {
            src,
            field_size,
            field_bits: field_size.trailing_zeros(),
            k_small,
            k_large,
            small: FastTable::new(table_log, k_small),
            large: use_large.then(|| FastTable::new(table_log, k_large)),
            cache: HashCache::default(),
            low_limit,
        }
    }

    const fn fs(&self) -> isize {
        self.field_size as isize
    }

    /// The first position the cache cannot hash: the long hash reads
    /// `k_large` bytes.
    const fn cache_limit(&self) -> isize {
        self.src.len() as isize - self.k_large as isize + 1
    }

    /// The slot of the hash of `ip` (`(ip - base) >> fieldBits`, with the
    /// window base one byte before the source).
    const fn slot(&self, ip: isize) -> usize {
        ((ip + 1) as usize) >> self.field_bits
    }

    /// `ZS_HashCache_init`.
    fn cache_init(&mut self, mut ip: isize) {
        self.cache = HashCache::default();
        let limit = self.cache_limit();
        let first = self.slot(ip);
        let mut i = 0;
        while i < HASH_SIZE && ip < limit {
            let at = ip as usize;
            self.cache.small[(first + i) & HASH_MASK] = self.small.hash(self.src, at) as u32;
            if let Some(large) = &self.large {
                self.cache.large[(first + i) & HASH_MASK] = large.hash(self.src, at) as u32;
            }
            i += 1;
            ip += self.fs();
        }
    }

    /// `ZS_HashCache_update`: the hashes of `ip`, then the slot is refilled
    /// with the hash of the field sixteen fields later.
    fn cache_update(&mut self, ip: isize) -> (u32, u32) {
        let slot = self.slot(ip) & HASH_MASK;
        let hashes = (
            self.cache.small[slot],
            if self.large.is_some() {
                self.cache.large[slot]
            } else {
                0
            },
        );
        let next = ip + self.fs() * HASH_SIZE as isize;
        if next < self.cache_limit() {
            let at = next as usize;
            self.cache.small[slot] = self.small.hash(self.src, at) as u32;
            if let Some(large) = &self.large {
                self.cache.large[slot] = large.hash(self.src, at) as u32;
            }
        }
        self.cache.next = Some(ip + self.fs());
        hashes
    }

    /// `ZS_HashCache_skip`: catches the cache up to `ip` after a match.
    fn cache_skip(&mut self, ip: isize) {
        let skip = self
            .cache
            .next
            .map_or(usize::MAX, |prev| (ip - prev) as usize);
        if skip >= HASH_SIZE * self.field_size as usize {
            self.cache_init(ip);
            return;
        }
        let mut prev = self.cache.next.unwrap_or(0);
        while prev < ip {
            self.cache_update(prev);
            prev += self.fs();
        }
    }

    /// `findMatchDFast`: the long candidate first, then the short one.
    /// Returns the match position and the sequence (literals up to `ip`).
    fn find_match(
        &mut self,
        hashes: (u32, u32),
        anchor: isize,
        ip: isize,
    ) -> Option<(isize, Sequence)> {
        let index = (ip + 1) as u32;
        if let Some(large) = &mut self.large {
            let pos = large.get_and_update_hash(hashes.1 as usize, index);
            let offset = index.wrapping_sub(pos);
            if pos >= self.low_limit && offset < MAX_OFFSET {
                let matched = pos as isize - 1;
                if check_match(self.src, ip, matched, self.k_small) {
                    return Some((
                        matched,
                        Sequence {
                            rep: false,
                            match_code: offset >> self.field_bits,
                            literal_length: (ip - anchor) as u32,
                            match_length: count_fields(self.src, ip, matched, self.field_size)
                                as u32,
                        },
                    ));
                }
            }
        }
        let pos = self.small.get_and_update_hash(hashes.0 as usize, index);
        let offset = index.wrapping_sub(pos);
        if pos >= self.low_limit && offset < MAX_OFFSET {
            let matched = pos as isize - 1;
            if check_match(self.src, ip, matched, self.k_small) {
                return Some((
                    matched,
                    Sequence {
                        rep: false,
                        match_code: offset >> self.field_bits,
                        literal_length: (ip - anchor) as u32,
                        match_length: count_fields(self.src, ip, matched, self.field_size) as u32,
                    },
                ));
            }
        }
        None
    }
}

/// `ZS_tokenLzMatchFinder_parseT`: parses `src`, a whole number of fields
/// of `field_size` (2, 4 or 8) bytes, into sequences and literals.
pub(super) fn parse(
    src: &[u8],
    field_size: u32,
    table_log: u32,
    use_large: bool,
    low_limit: u32,
) -> SeqStore {
    let mut finder = Finder::new(src, field_size, table_log, use_large, low_limit);
    let mut store = SeqStore::default();
    let fs = finder.fs();
    let end = src.len() as isize;
    let limit = end - fs.max(16);
    let mut anchor: isize = 0;
    let mut rep = [field_size, 2 * field_size];
    let mut ip = rep[1] as isize;
    finder.cache_init(ip);

    while ip < limit {
        let mut found: Option<(isize, Sequence)> = None;
        loop {
            let hashes = finder.cache_update(ip);
            for (r, &distance) in rep.iter().enumerate() {
                let matched = ip - distance as isize;
                if check_match(src, ip, matched, finder.k_small) {
                    found = Some((
                        matched,
                        Sequence {
                            rep: true,
                            match_code: r as u32,
                            literal_length: (ip - anchor) as u32,
                            match_length: count_fields(src, ip, matched, field_size) as u32,
                        },
                    ));
                    break;
                }
            }
            if found.is_none() {
                found = finder.find_match(hashes, anchor, ip);
            }
            if found.is_some() {
                break;
            }
            ip += fs;
            if ip >= limit {
                break;
            }
        }
        let Some((mut matched, mut seq)) = found else {
            continue;
        };

        ip = anchor + seq.literal_length as isize;
        // Roll the match back over equal fields, down to the anchor and the
        // start of the window.
        while ip > anchor && matched > fs - 1 && check_match(src, ip - fs, matched - fs, field_size)
        {
            ip -= fs;
            matched -= fs;
            seq.match_length += field_size;
            seq.literal_length -= field_size;
        }

        if seq.rep {
            if seq.match_code != 0 {
                let code = seq.match_code as usize;
                let offset = rep[code];
                rep.copy_within(0..code, 1);
                rep[0] = offset;
                seq.match_code = seq.match_code.min(1);
            }
        } else {
            rep[1] = rep[0];
            rep[0] = seq.match_code << finder.field_bits;
        }
        store.store(src, anchor as usize, seq);

        let length = seq.match_length as isize;
        if ip + length <= limit && length > fs {
            let start = ip;
            let stop = ip + length;
            let start_index = (start + 1) as u32;
            let stop_index = (stop + 1) as u32;
            let step = field_size;
            finder
                .small
                .put(src, (start + fs) as usize, start_index + step);
            finder
                .small
                .put(src, (stop - fs) as usize, stop_index - step);
            if let Some(large) = &mut finder.large {
                large.put(src, (start + fs) as usize, start_index + step);
                large.put(src, (stop - 2 * fs) as usize, stop_index - 2 * step);
            }
            finder.cache_skip(stop);
        }
        ip += length;
        anchor = ip;
    }
    store.store_last_literals(src, anchor as usize);
    store
}
