//! The merge kernels of the PivCo-Huffman decoder (`arch/decode_pivco_*.c`).
//!
//! [`merge`] interleaves the symbols of the two children of a tree node by the
//! node's bitmap and [`flat`] expands a flat leaf. Each has a scalar baseline
//! (used at [`Level::Fallback`]) and `fearless_simd` kernels of two widths:
//! the AVX-512 level merges 64 output bytes per step with `expand` (the
//! reference's `vpexpandb`), every other level 16 bytes per step with two
//! `swizzle_dyn` gathers whose controls come from two 256-entry tables (the
//! reference's `pshufb` merge). All produce identical bytes.
//!
//! The kernels take the SIMD token the walker was dispatched with, so the
//! recursion over the tree runs inside one dispatched region and a kernel
//! costs no level match.

use fearless_simd::{Bytes, Level, Simd, SimdBase, dispatch, i8x16, mask8x16, u8x16, u8x64, u16x8};

use super::Part;

/// The bytes a vector source holds past its symbols: the merge kernels read
/// their sources a whole step at a time (the widest step is 64 bytes).
pub(super) const SLACK: usize = 64;

/// Whether `simd` is the AVX-512 level, whose merge is 64 bytes wide.
#[inline(always)]
fn is_wide<S: Simd>(simd: S) -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        matches!(simd.level(), Level::Avx512(_))
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        let _ = simd;
        false
    }
}

/// Interleaves two parts into `out` by `bitmap`: a set bit takes the next
/// symbol of `right`, a clear bit of `left`. Returns the number of set bits
/// (`mergeGeneric`). A source asked for more symbols than it holds yields
/// zeros; the caller catches that through the returned count.
pub(super) fn merge(
    level: Level,
    out: &mut [u8],
    bitmap: &[u8],
    left: Part<'_>,
    right: Part<'_>,
) -> usize {
    if level.is_fallback() {
        merge_scalar(out, bitmap, left, right)
    } else {
        dispatch!(level, simd => merge_simd(simd, out, bitmap, left, right))
    }
}

/// The `fearless_simd` kernel of [`merge`]: the widest the level supports.
#[inline(always)]
fn merge_simd<S: Simd>(
    simd: S,
    out: &mut [u8],
    bitmap: &[u8],
    left: Part<'_>,
    right: Part<'_>,
) -> usize {
    if is_wide(simd) {
        merge_wide(simd, out, bitmap, left, right)
    } else {
        merge_narrow(simd, out, bitmap, left, right)
    }
}

/// Expands a flat leaf: one `depth`-bit index per output byte into `out`
/// (`mergeFlatDepth`). `symbols` has `2^depth` entries.
pub(super) fn flat(level: Level, out: &mut [u8], bitmap: &[u8], depth: usize, symbols: &[u8]) {
    if level.is_fallback() {
        flat_scalar(out, bitmap, depth, symbols)
    } else {
        dispatch!(level, simd => flat_simd(simd, out, bitmap, depth, symbols))
    }
}

/// A source consumed in order while merging.
struct Source<'a> {
    part: Part<'a>,
    taken: usize,
}

impl Source<'_> {
    fn next(&mut self) -> u8 {
        let value = match self.part {
            Part::Constant(symbol) => symbol,
            Part::Vector(symbols) => symbols.get(self.taken).copied().unwrap_or(0),
        };
        self.taken += 1;
        value
    }
}

/// The scalar baseline of [`merge`]: one symbol per bit.
pub(super) fn merge_scalar(
    out: &mut [u8],
    bitmap: &[u8],
    left: Part<'_>,
    right: Part<'_>,
) -> usize {
    let mut left = Source {
        part: left,
        taken: 0,
    };
    let mut right = Source {
        part: right,
        taken: 0,
    };
    let mut ones = 0;
    for (chunk, &byte) in out.chunks_mut(8).zip(bitmap) {
        for (bit, slot) in chunk.iter_mut().enumerate() {
            if byte >> bit & 1 != 0 {
                ones += 1;
                *slot = right.next();
            } else {
                *slot = left.next();
            }
        }
    }
    ones
}

/// The scalar baseline of [`flat`]: eight indices per 64-bit window.
pub(super) fn flat_scalar(out: &mut [u8], bitmap: &[u8], depth: usize, symbols: &[u8]) {
    match depth {
        1 => flat_window::<1>(out, bitmap, symbols),
        2 => flat_window::<2>(out, bitmap, symbols),
        3 => flat_window::<3>(out, bitmap, symbols),
        4 => flat_window::<4>(out, bitmap, symbols),
        5 => flat_window::<5>(out, bitmap, symbols),
        6 => flat_window::<6>(out, bitmap, symbols),
        7 => flat_window::<7>(out, bitmap, symbols),
        _ => flat_bytes(out, bitmap, symbols),
    }
}

/// [`flat_scalar`] at depth 8: each index is a byte of the bitmap.
#[inline(always)]
fn flat_bytes(out: &mut [u8], bitmap: &[u8], symbols: &[u8]) {
    let table = symbol_table(symbols);
    for (slot, &index) in out.iter_mut().zip(bitmap) {
        *slot = table[usize::from(index)];
    }
    // A short bitmap (corruption cannot reach here) leaves zeros.
    if let Some(rest) = out.get_mut(bitmap.len()..) {
        rest.fill(table[0]);
    }
}

/// [`flat_scalar`] at a depth known at compile time.
#[inline(always)]
fn flat_window<const DEPTH: usize>(out: &mut [u8], bitmap: &[u8], symbols: &[u8]) {
    let table = symbol_table(symbols);
    let mask = ((1u16 << DEPTH) - 1) as u8;
    let full = out.len() / 8;
    // Eight indices of `DEPTH` bits occupy exactly `DEPTH` bytes.
    let (body, tail) = out.split_at_mut(full * 8);
    let (packed, rest) = bitmap.split_at(bitmap.len().min(full * DEPTH));
    let (groups, _) = body.as_chunks_mut::<8>();
    let (fields, _) = packed.as_chunks::<DEPTH>();
    for (symbols, bytes) in groups.iter_mut().zip(fields) {
        let mut window = window::<DEPTH>(bytes);
        for slot in symbols {
            *slot = table[usize::from(window as u8 & mask)];
            window >>= DEPTH;
        }
    }
    let mut window = window::<DEPTH>(rest);
    for slot in tail {
        *slot = table[usize::from(window as u8 & mask)];
        window >>= DEPTH;
    }
}

/// The little-endian window of up to `DEPTH` bytes, zero padded.
#[inline(always)]
fn window<const DEPTH: usize>(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    match bytes.first_chunk::<DEPTH>() {
        Some(exact) => word[..DEPTH].copy_from_slice(exact),
        None => word[..bytes.len()].copy_from_slice(bytes),
    }
    u64::from_le_bytes(word)
}

/// `symbols` as a table indexable by any byte.
fn symbol_table(symbols: &[u8]) -> [u8; 256] {
    let mut table = [0u8; 256];
    let len = symbols.len().min(256);
    table[..len].copy_from_slice(&symbols[..len]);
    table
}

/// `N` bytes of `source` from `at`, if it holds that many.
#[inline(always)]
fn chunk<const N: usize>(source: &[u8], at: usize) -> Option<&[u8; N]> {
    source.get(at..)?.first_chunk::<N>()
}

/// `N` bytes of `source` from `at`, zero padded past its end.
#[cold]
#[inline(never)]
fn edge<const N: usize>(source: &[u8], at: usize) -> [u8; N] {
    let mut bytes = [0u8; N];
    let rest = source.get(at..).unwrap_or_default();
    let len = rest.len().min(N);
    bytes[..len].copy_from_slice(&rest[..len]);
    bytes
}

/// Sixteen bytes of `source` from `at`, zero padded past its end.
#[inline(always)]
fn load<S: Simd>(simd: S, source: &[u8], at: usize) -> u8x16<S> {
    match chunk::<16>(source, at) {
        Some(bytes) => u8x16::load_array_ref(simd, bytes),
        None => u8x16::load_array(simd, edge(source, at)),
    }
}

/// Sixty-four bytes of `source` from `at`, zero padded past its end.
#[inline(always)]
fn load_wide<S: Simd>(simd: S, source: &[u8], at: usize) -> u8x64<S> {
    match chunk::<64>(source, at) {
        Some(bytes) => u8x64::load_array_ref(simd, bytes),
        None => u8x64::load_array(simd, edge(source, at)),
    }
}

/// Runs `step` over `out` sixteen bytes at a time with the matching 16 bits
/// of the bitmap (bits past the end of `out` are cleared). Returns the number
/// of set bits.
#[inline(always)]
fn drive<S: Simd>(out: &mut [u8], bitmap: &[u8], mut step: impl FnMut(u16) -> u8x16<S>) -> usize {
    let out_len = out.len();
    let mut ones = 0usize;
    let (chunks, tail) = out.as_chunks_mut::<16>();
    let (words, _) = bitmap.as_chunks::<2>();
    for (chunk, word) in chunks.iter_mut().zip(words) {
        let bits = u16::from_le_bytes(*word);
        ones += bits.count_ones() as usize;
        step(bits).store_slice(chunk);
    }
    if !tail.is_empty() {
        let bytes = bitmap.get(out_len / 16 * 2..).unwrap_or_default();
        let mut word = [0u8; 2];
        let len = bytes.len().min(2);
        word[..len].copy_from_slice(&bytes[..len]);
        let bits = u16::from_le_bytes(word) & ((1u16 << tail.len()) - 1);
        ones += bits.count_ones() as usize;
        let lanes: [u8; 16] = step(bits).into();
        tail.copy_from_slice(&lanes[..tail.len()]);
    }
    ones
}

/// [`drive`] sixty-four bytes at a time.
#[inline(always)]
fn drive_wide<S: Simd>(
    out: &mut [u8],
    bitmap: &[u8],
    mut step: impl FnMut(u64) -> u8x64<S>,
) -> usize {
    let out_len = out.len();
    let mut ones = 0usize;
    let (chunks, tail) = out.as_chunks_mut::<64>();
    let (words, _) = bitmap.as_chunks::<8>();
    for (chunk, word) in chunks.iter_mut().zip(words) {
        let bits = u64::from_le_bytes(*word);
        ones += bits.count_ones() as usize;
        step(bits).store_slice(chunk);
    }
    if !tail.is_empty() {
        let bytes = bitmap.get(out_len / 64 * 8..).unwrap_or_default();
        let mut word = [0u8; 8];
        let len = bytes.len().min(8);
        word[..len].copy_from_slice(&bytes[..len]);
        let bits = u64::from_le_bytes(word) & ((1u64 << tail.len()) - 1);
        ones += bits.count_ones() as usize;
        let lanes: [u8; 64] = step(bits).into();
        tail.copy_from_slice(&lanes[..tail.len()]);
    }
    ones
}

/// The merge controls of the low and the high byte of a 16-bit bitmap word.
/// Summed they give, per lane `L` with `P` set bits before it, the index into
/// the right source (`P`) at a set lane and `0xFF - (L - P)` at a clear one,
/// whose high bit zeroes the lane in a `pshufb`; XOR with `0xFF` turns that
/// into the left source's index (`L - P`) with the set lanes zeroed.
///
/// The low table holds the controls of lanes 0 to 7 and, in its upper half,
/// the set bits of the byte, which the sum carries into lanes 8 to 15; the
/// high table holds the lane-local controls of lanes 8 to 15.
const CONTROLS: ([[u8; 16]; 256], [[u8; 16]; 256]) = {
    let mut low = [[0u8; 16]; 256];
    let mut high = [[0u8; 16]; 256];
    let mut byte = 0usize;
    while byte < 256 {
        let ones = (byte as u8).count_ones() as u8;
        let mut lane = 0usize;
        let mut before = 0u8;
        while lane < 8 {
            let set = (byte >> lane) & 1 != 0;
            low[byte][lane] = if set {
                before
            } else {
                0xFF - (lane as u8 - before)
            };
            low[byte][8 + lane] = ones;
            high[byte][8 + lane] = if set {
                before
            } else {
                0xFF - (8 + lane as u8 - before)
            };
            before += set as u8;
            lane += 1;
        }
        byte += 1;
    }
    (low, high)
};

/// The merge control of the sixteen bits of `bits` (see [`CONTROLS`]).
#[inline(always)]
fn control<S: Simd>(simd: S, bits: u16) -> u8x16<S> {
    let low = u8x16::load_array_ref(simd, &CONTROLS.0[usize::from(bits as u8)]);
    let high = u8x16::load_array_ref(simd, &CONTROLS.1[usize::from(bits >> 8)]);
    simd.add_u8x16(low, high)
}

/// A byte shuffle in which a control with the high bit set (or above 15)
/// yields zero. The SSE4.2 and AVX2 levels (`pshufb`) do that natively; the others
/// (`swizzle_dyn` leaves out-of-range indices implementation-defined) take the
/// precise variant.
#[inline(always)]
fn shuffle<S: Simd>(simd: S, bytes: u8x16<S>, control: u8x16<S>) -> u8x16<S> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if matches!(simd.level(), Level::Sse4_2(_) | Level::Avx2(_)) {
        return simd.swizzle_dyn_u8x16(bytes, control);
    }
    simd.swizzle_dyn_precise_u8x16(bytes, control)
}

/// The lanes of `control` that name no source byte, as a blend mask.
#[inline(always)]
fn unselected<S: Simd>(simd: S, control: u8x16<S>) -> mask8x16<S> {
    simd.simd_lt_i8x16(control.bitcast::<i8x16<S>>(), simd.splat_i8x16(0))
}

/// The 16-byte kernel of [`merge`].
#[inline(always)]
fn merge_narrow<S: Simd>(
    simd: S,
    out: &mut [u8],
    bitmap: &[u8],
    left: Part<'_>,
    right: Part<'_>,
) -> usize {
    match (left, right) {
        (Part::Vector(left), Part::Vector(right)) => {
            let (mut taken_left, mut taken_right) = (0usize, 0usize);
            let all = simd.splat_u8x16(0xFF);
            drive(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let control = control(simd, bits);
                    let from_right = shuffle(simd, load(simd, right, taken_right), control);
                    let from_left = shuffle(
                        simd,
                        load(simd, left, taken_left),
                        simd.xor_u8x16(control, all),
                    );
                    let ones = bits.count_ones() as usize;
                    taken_right += ones;
                    taken_left += 16 - ones;
                    simd.or_u8x16(from_right, from_left)
                },
            )
        }
        (Part::Constant(symbol), Part::Vector(right)) => {
            let fill = simd.splat_u8x16(symbol);
            let mut taken = 0usize;
            drive(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let control = control(simd, bits);
                    let from_right = shuffle(simd, load(simd, right, taken), control);
                    taken += bits.count_ones() as usize;
                    simd.select_u8x16(unselected(simd, control), fill, from_right)
                },
            )
        }
        (Part::Vector(left), Part::Constant(symbol)) => {
            let fill = simd.splat_u8x16(symbol);
            let all = simd.splat_u8x16(0xFF);
            let mut taken = 0usize;
            drive(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let control = simd.xor_u8x16(control(simd, bits), all);
                    let from_left = shuffle(simd, load(simd, left, taken), control);
                    taken += 16 - bits.count_ones() as usize;
                    simd.select_u8x16(unselected(simd, control), fill, from_left)
                },
            )
        }
        (Part::Constant(left), Part::Constant(right)) => {
            let (left, right) = (simd.splat_u8x16(left), simd.splat_u8x16(right));
            drive(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let mask = simd.from_bitmask_mask8x16(u64::from(bits));
                    simd.select_u8x16(mask, right, left)
                },
            )
        }
    }
}

/// The 64-byte kernel of [`merge`]: `expand` scatters each source's next
/// symbols into the lanes its mask selects.
#[inline(always)]
fn merge_wide<S: Simd>(
    simd: S,
    out: &mut [u8],
    bitmap: &[u8],
    left: Part<'_>,
    right: Part<'_>,
) -> usize {
    match (left, right) {
        (Part::Vector(left), Part::Vector(right)) => {
            let (mut taken_left, mut taken_right) = (0usize, 0usize);
            drive_wide(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let ones = bits.count_ones() as usize;
                    let from_left = simd.expand_u8x64(
                        load_wide(simd, left, taken_left),
                        simd.from_bitmask_mask8x64(!bits),
                    );
                    let merged = simd.expand_merge_u8x64(
                        load_wide(simd, right, taken_right),
                        simd.from_bitmask_mask8x64(bits),
                        from_left,
                    );
                    taken_right += ones;
                    taken_left += 64 - ones;
                    merged
                },
            )
        }
        (Part::Constant(symbol), Part::Vector(right)) => {
            let fill = simd.splat_u8x64(symbol);
            let mut taken = 0usize;
            drive_wide(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let merged = simd.expand_merge_u8x64(
                        load_wide(simd, right, taken),
                        simd.from_bitmask_mask8x64(bits),
                        fill,
                    );
                    taken += bits.count_ones() as usize;
                    merged
                },
            )
        }
        (Part::Vector(left), Part::Constant(symbol)) => {
            let fill = simd.splat_u8x64(symbol);
            let mut taken = 0usize;
            drive_wide(
                out,
                bitmap,
                #[inline(always)]
                |bits| {
                    let merged = simd.expand_merge_u8x64(
                        load_wide(simd, left, taken),
                        simd.from_bitmask_mask8x64(!bits),
                        fill,
                    );
                    taken += 64 - bits.count_ones() as usize;
                    merged
                },
            )
        }
        (Part::Constant(left), Part::Constant(right)) => {
            let (left, right) = (simd.splat_u8x64(left), simd.splat_u8x64(right));
            drive_wide(
                out,
                bitmap,
                #[inline(always)]
                |bits| simd.select_u8x64(simd.from_bitmask_mask8x64(bits), right, left),
            )
        }
    }
}

/// The `fearless_simd` kernel of [`flat`]: sixteen indices per step, unpacked
/// into byte lanes and looked up with `swizzle_dyn` (tables of up to 128
/// entries; depth 8 is a byte lookup).
#[inline(always)]
fn flat_simd<S: Simd>(simd: S, out: &mut [u8], bitmap: &[u8], depth: usize, symbols: &[u8]) {
    match depth {
        1 => flat_vector::<S, 1>(simd, out, bitmap, symbols),
        2 => flat_vector::<S, 2>(simd, out, bitmap, symbols),
        3 => flat_vector::<S, 3>(simd, out, bitmap, symbols),
        4 => flat_vector::<S, 4>(simd, out, bitmap, symbols),
        5 => flat_vector::<S, 5>(simd, out, bitmap, symbols),
        6 => flat_vector::<S, 6>(simd, out, bitmap, symbols),
        7 => flat_vector::<S, 7>(simd, out, bitmap, symbols),
        _ => flat_bytes(out, bitmap, symbols),
    }
}

/// The unpacking constants of [`flat_vector`] at one depth.
///
/// Sixteen `DEPTH`-bit fields span `2 * DEPTH` bytes. Each field lies in two
/// adjacent bytes, which a `swizzle_dyn` gathers into a 16-bit lane; a
/// multiply by `2^(8 - shift)` moves the field to the high byte, which a
/// shift and a mask isolate. Per half of the sixteen fields: the two source
/// bytes of each lane and the multiplier that aligns its field.
struct Fields<const DEPTH: usize>;

impl<const DEPTH: usize> Fields<DEPTH> {
    const GATHER: [[u8; 16]; 2] = {
        let mut gather = [[0u8; 16]; 2];
        let mut half = 0;
        while half < 2 {
            let mut lane = 0;
            while lane < 8 {
                let byte = (half * 8 + lane) * DEPTH / 8;
                gather[half][2 * lane] = byte as u8;
                gather[half][2 * lane + 1] = if byte + 1 > 15 { 15 } else { byte as u8 + 1 };
                lane += 1;
            }
            half += 1;
        }
        gather
    };
    const MULTIPLIER: [[u16; 8]; 2] = {
        let mut multiplier = [[0u16; 8]; 2];
        let mut half = 0;
        while half < 2 {
            let mut lane = 0;
            while lane < 8 {
                multiplier[half][lane] = 1 << (8 - (half * 8 + lane) * DEPTH % 8);
                lane += 1;
            }
            half += 1;
        }
        multiplier
    };
}

/// The constants of [`flat_vector`] at one depth.
struct Unpack<S: Simd, const DEPTH: usize> {
    simd: S,
    gather: [u8x16<S>; 2],
    multiplier: [u16x8<S>; 2],
    field: u16x8<S>,
    pages: [u8x16<S>; 8],
    low: u8x16<S>,
}

impl<S: Simd, const DEPTH: usize> Unpack<S, DEPTH> {
    /// The sixteen fields of `packed` as byte lanes.
    #[inline(always)]
    fn indices(&self, packed: u8x16<S>) -> u8x16<S> {
        let simd = self.simd;
        let mut halves = [simd.splat_u16x8(0); 2];
        for (half, lanes) in halves.iter_mut().enumerate() {
            let gathered: u16x8<S> = simd.swizzle_dyn_u8x16(packed, self.gather[half]).bitcast();
            let aligned = simd.shr_u16x8(simd.mul_u16x8(gathered, self.multiplier[half]), 8);
            *lanes = simd.and_u16x8(aligned, self.field);
        }
        simd.narrow_u16x8(halves[0], halves[1])
    }

    /// The symbols of `indices`, from pages of sixteen table entries.
    #[inline(always)]
    fn lookup(&self, indices: u8x16<S>) -> u8x16<S> {
        let simd = self.simd;
        let within = simd.and_u8x16(indices, self.low);
        let mut found = simd.swizzle_dyn_u8x16(self.pages[0], within);
        if DEPTH > 4 {
            let page_of = simd.shr_u8x16(indices, 4);
            for (page, table) in self.pages.iter().enumerate().take(1 << (DEPTH - 4)).skip(1) {
                let wanted = simd.simd_eq_u8x16(page_of, simd.splat_u8x16(page as u8));
                found = simd.select_u8x16(wanted, simd.swizzle_dyn_u8x16(*table, within), found);
            }
        }
        found
    }
}

/// [`flat_simd`] at a depth of 1 to 7 known at compile time.
#[inline(always)]
fn flat_vector<S: Simd, const DEPTH: usize>(
    simd: S,
    out: &mut [u8],
    bitmap: &[u8],
    symbols: &[u8],
) {
    let gather = Fields::<DEPTH>::GATHER
        .each_ref()
        .map(|bytes| u8x16::load_array_ref(simd, bytes));
    let multiplier = Fields::<DEPTH>::MULTIPLIER
        .each_ref()
        .map(|lanes| u16x8::load_array_ref(simd, lanes));
    // The symbol table in pages of sixteen entries.
    let mut pages = [simd.splat_u8x16(0); 8];
    let (whole, rest) = symbols.as_chunks::<16>();
    for (page, entries) in pages.iter_mut().zip(whole) {
        *page = u8x16::load_array_ref(simd, entries);
    }
    if whole.is_empty() {
        pages[0] = u8x16::load_array(simd, edge(rest, 0));
    }

    let unpack = Unpack::<S, DEPTH> {
        simd,
        gather,
        multiplier,
        field: simd.splat_u16x8((1 << DEPTH) - 1),
        pages,
        low: simd.splat_u8x16(0x0F),
    };

    let step_bytes = 2 * DEPTH;
    let full = out.len() / 16;
    let (chunks, tail) = out.as_chunks_mut::<16>();
    for (step, chunk) in chunks.iter_mut().enumerate() {
        let packed = load(simd, bitmap, step * step_bytes);
        unpack.lookup(unpack.indices(packed)).store_slice(chunk);
    }
    if !tail.is_empty() {
        let packed = load(simd, bitmap, full * step_bytes);
        let lanes: [u8; 16] = unpack.lookup(unpack.indices(packed)).into();
        tail.copy_from_slice(&lanes[..tail.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;
    use alloc::vec::Vec;

    fn pattern(len: usize, seed: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i.wrapping_mul(0x9E37_79B1).wrapping_add(seed) >> 13) as u8)
            .collect()
    }

    #[test]
    fn merge_matches_the_scalar_baseline_at_every_level() {
        let lengths: &[usize] = if cfg!(miri) {
            &[0, 1, 15, 16, 17, 33, 70]
        } else {
            &[0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 100, 1000]
        };
        for &len in lengths {
            let bitmap = pattern(len.div_ceil(8), 5);
            let ones = bitmap
                .iter()
                .enumerate()
                .map(|(at, &byte)| {
                    let live = (len - at * 8).min(8);
                    (byte & ((1u16 << live) - 1) as u8).count_ones() as usize
                })
                .sum::<usize>();
            let left = pattern(len - ones, 1);
            let right = pattern(ones, 2);
            let parts = [
                (Part::Vector(&left[..]), Part::Vector(&right[..])),
                (Part::Constant(9), Part::Vector(&right[..])),
                (Part::Vector(&left[..]), Part::Constant(4)),
                (Part::Constant(9), Part::Constant(4)),
                // Sources shorter than the bitmap asks for (corruption).
                (
                    Part::Vector(&left[..left.len() / 2]),
                    Part::Vector(&right[..right.len() / 2]),
                ),
            ];
            for (left, right) in parts {
                let mut expected = alloc::vec![0u8; len];
                let want = merge_scalar(&mut expected, &bitmap, left, right);
                for backend in Backend::available() {
                    let mut got = alloc::vec![0u8; len];
                    let ones = merge(backend.0, &mut got, &bitmap, left, right);
                    assert_eq!(ones, want, "{backend}, length {len}");
                    assert_eq!(got, expected, "{backend}, length {len}");
                }
            }
        }
    }

    #[test]
    fn flat_matches_the_scalar_baseline_at_every_level() {
        for depth in 1..=8usize {
            let symbols = pattern(1 << depth, depth);
            for len in [0usize, 1, 7, 8, 9, 15, 16, 17, 63, 64, 65, 300] {
                let bitmap = pattern((len * depth).div_ceil(8), 11);
                let mut expected = alloc::vec![0u8; len];
                flat_scalar(&mut expected, &bitmap, depth, &symbols);
                // Index by index, the definition.
                for (at, &byte) in expected.iter().enumerate() {
                    let mut index = 0usize;
                    for bit in 0..depth {
                        let position = at * depth + bit;
                        index |= usize::from(bitmap[position / 8] >> (position % 8) & 1) << bit;
                    }
                    assert_eq!(byte, symbols[index], "depth {depth}, length {len}, at {at}");
                }
                for backend in Backend::available() {
                    let mut got = alloc::vec![0u8; len];
                    flat(backend.0, &mut got, &bitmap, depth, &symbols);
                    assert_eq!(got, expected, "{backend}, depth {depth}, length {len}");
                }
            }
        }
    }
}
