//! FSE compression (`fse/compress/fse_compress.c`, `fse/fse.h`).
//!
//! The pipeline an FSE node runs: [`normalize_count`] turns a histogram into
//! normalised counts, [`CTable::build`] spreads symbols over the state table,
//! [`write_ncount`] serialises the counts and [`compress_using_ctable`]
//! encodes with 2 or 4 interleaved states (OpenZL's change to the library).

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use fearless_simd::{Level, dispatch};

use super::bits::WordWriter;
use super::{EntropyError, highbit32};

/// `FSE_MIN_TABLELOG`.
pub(crate) const MIN_TABLELOG: u32 = 5;
/// `FSE_MAX_TABLELOG`.
pub(crate) const MAX_TABLELOG: u32 = 12;
/// `FSE_DEFAULT_TABLELOG`.
pub(crate) const DEFAULT_TABLELOG: u32 = 11;
/// `FSE_NCOUNTBOUND`: the room a count header always fits in.
pub(crate) const NCOUNT_BOUND: usize = 512;
/// `FSE_MAX_SYMBOL_VALUE`.
pub(crate) const MAX_SYMBOL_VALUE: u32 = 255;

/// `FSE_BLOCKBOUND`.
pub(crate) const fn block_bound(size: usize) -> usize {
    size + (size >> 7) + 4 + core::mem::size_of::<usize>()
}

/// `FSE_compressBound` (`FSE_COMPRESSBOUND`).
pub(crate) const fn compress_bound(size: usize) -> usize {
    NCOUNT_BOUND + block_bound(size)
}

/// `FSE_TABLESTEP`.
const fn table_step(table_size: u32) -> u32 {
    (table_size >> 1) + (table_size >> 3) + 3
}

/// `FSE_minTableLog`: the smallest log that safely represents the
/// distribution.
fn min_table_log(src_size: usize, max_symbol_value: u32) -> u32 {
    let min_bits_src = highbit32(src_size as u32) + 1;
    let min_bits_symbols = highbit32(max_symbol_value) + 2;
    min_bits_src.min(min_bits_symbols)
}

/// `FSE_optimalTableLog_internal`.
pub(crate) fn optimal_table_log_internal(
    max_table_log: u32,
    src_size: usize,
    max_symbol_value: u32,
    minus: u32,
) -> u32 {
    // Unsigned arithmetic as in the reference: a tiny source wraps `max_bits_src`
    // and leaves the requested log alone.
    let max_bits_src = highbit32((src_size as u32).wrapping_sub(1)).wrapping_sub(minus);
    let mut table_log = max_table_log;
    let min_bits = min_table_log(src_size, max_symbol_value);
    if table_log == 0 {
        table_log = DEFAULT_TABLELOG;
    }
    if max_bits_src < table_log {
        table_log = max_bits_src;
    }
    if min_bits > table_log {
        table_log = min_bits;
    }
    table_log.clamp(MIN_TABLELOG, MAX_TABLELOG)
}

/// `FSE_optimalTableLog`.
pub(crate) fn optimal_table_log(max_table_log: u32, src_size: usize, max_symbol_value: u32) -> u32 {
    optimal_table_log_internal(max_table_log, src_size, max_symbol_value, 2)
}

/// `FSE_normalizeM2`: the secondary normalisation, used when the primary
/// one cannot place the rounding error.
fn normalize_m2(
    norm: &mut [i16],
    table_log: u32,
    count: &[u32],
    mut total: usize,
    max_symbol_value: u32,
    low_prob_count: i16,
) -> Result<(), EntropyError> {
    const NOT_YET_ASSIGNED: i16 = -2;
    let mut distributed: u32 = 0;
    let low_threshold = (total >> table_log) as u32;
    let mut low_one = ((total * 3) >> (table_log + 1)) as u32;

    for (slot, &c) in norm.iter_mut().zip(&count[..=max_symbol_value as usize]) {
        if c == 0 {
            *slot = 0;
            continue;
        }
        if c <= low_threshold {
            *slot = low_prob_count;
            distributed += 1;
            total -= c as usize;
            continue;
        }
        if c <= low_one {
            *slot = 1;
            distributed += 1;
            total -= c as usize;
            continue;
        }
        *slot = NOT_YET_ASSIGNED;
    }
    let mut to_distribute = (1u32 << table_log) - distributed;

    if to_distribute == 0 {
        return Ok(());
    }

    if total / to_distribute as usize > low_one as usize {
        // Risk of rounding to zero.
        low_one = ((total * 3) / (to_distribute as usize * 2)) as u32;
        for s in 0..=max_symbol_value as usize {
            if norm[s] == NOT_YET_ASSIGNED && count[s] <= low_one {
                norm[s] = 1;
                distributed += 1;
                total -= count[s] as usize;
            }
        }
        to_distribute = (1u32 << table_log) - distributed;
    }

    if distributed == max_symbol_value + 1 {
        // All values are poor: give the remaining points to the maximum.
        let mut max_v = 0usize;
        let mut max_c = 0u32;
        for (s, &c) in count[..=max_symbol_value as usize].iter().enumerate() {
            if c > max_c {
                max_v = s;
                max_c = c;
            }
        }
        norm[max_v] = norm[max_v].wrapping_add(to_distribute as i16);
        return Ok(());
    }

    if total == 0 {
        // Everything was low enough for the low thresholds.
        let mut s = 0usize;
        while to_distribute > 0 {
            if norm[s] > 0 {
                to_distribute -= 1;
                norm[s] += 1;
            }
            s = (s + 1) % (max_symbol_value as usize + 1);
        }
        return Ok(());
    }

    let v_step_log = 62 - u64::from(table_log);
    let mid = (1u64 << (v_step_log - 1)) - 1;
    let r_step =
        (((1u64 << v_step_log) * u64::from(to_distribute)) + mid) / u64::from(total as u32);
    let mut tmp_total = mid;
    for (slot, &c) in norm.iter_mut().zip(&count[..=max_symbol_value as usize]) {
        if *slot == NOT_YET_ASSIGNED {
            let end = tmp_total.wrapping_add(u64::from(c).wrapping_mul(r_step));
            let s_start = (tmp_total >> v_step_log) as u32;
            let s_end = (end >> v_step_log) as u32;
            let weight = s_end.wrapping_sub(s_start);
            if weight < 1 {
                return Err(EntropyError::Generic);
            }
            *slot = weight as i16;
            tmp_total = end;
        }
    }
    Ok(())
}

/// `FSE_normalizeCount`: normalises `count[0..=max_symbol_value]` over
/// `total` samples into `norm`, summing to `1 << table_log`.
///
/// Returns the table log (0 for a constant source, which the reference
/// returns before touching the remaining entries).
///
/// # Errors
///
/// A table log outside `5..=12`, or one too small for the distribution.
pub(crate) fn normalize_count(
    norm: &mut [i16],
    mut table_log: u32,
    count: &[u32],
    total: usize,
    max_symbol_value: u32,
    use_low_prob_count: bool,
) -> Result<u32, EntropyError> {
    const RTB_TABLE: [u64; 8] = [
        0, 473_195, 504_333, 520_860, 550_000, 700_000, 750_000, 830_000,
    ];
    if table_log == 0 {
        table_log = DEFAULT_TABLELOG;
    }
    if table_log < MIN_TABLELOG {
        return Err(EntropyError::Generic);
    }
    if table_log > MAX_TABLELOG {
        return Err(EntropyError::TableLogTooLarge);
    }
    if table_log < min_table_log(total, max_symbol_value) {
        return Err(EntropyError::Generic);
    }

    let low_prob_count: i16 = if use_low_prob_count { -1 } else { 1 };
    let scale = 62 - u64::from(table_log);
    let step = (1u64 << 62) / u64::from(total as u32);
    let v_step = 1u64 << (scale - 20);
    let mut still_to_distribute: i32 = 1 << table_log;
    let mut largest = 0usize;
    let mut largest_p: i16 = 0;
    let low_threshold = (total >> table_log) as u32;

    for s in 0..=max_symbol_value as usize {
        if count[s] as usize == total {
            return Ok(0);
        }
        if count[s] == 0 {
            norm[s] = 0;
            continue;
        }
        if count[s] <= low_threshold {
            norm[s] = low_prob_count;
            still_to_distribute -= 1;
        } else {
            let scaled = u64::from(count[s]).wrapping_mul(step);
            let mut proba = (scaled >> scale) as i16;
            if proba < 8 {
                let rest_to_beat = v_step * RTB_TABLE[proba as usize];
                let remainder = scaled.wrapping_sub(u64::from(proba as u16) << scale);
                proba = proba.wrapping_add(i16::from(remainder > rest_to_beat));
            }
            if proba > largest_p {
                largest_p = proba;
                largest = s;
            }
            norm[s] = proba;
            still_to_distribute -= i32::from(proba);
        }
    }
    if -still_to_distribute >= i32::from(norm[largest] >> 1) {
        // Corner case: another normalisation method.
        normalize_m2(
            norm,
            table_log,
            count,
            total,
            max_symbol_value,
            low_prob_count,
        )?;
    } else {
        norm[largest] = norm[largest].wrapping_add(still_to_distribute as i16);
    }
    Ok(table_log)
}

/// `FSE_symbolCompressionTransform`.
#[derive(Clone, Copy, Debug, Default)]
struct SymbolTransform {
    delta_find_state: i32,
    delta_nb_bits: u32,
}

/// A compression table (`FSE_CTable`).
#[derive(Debug)]
pub(crate) struct CTable {
    table_log: u32,
    /// `2^table_log` states, in a fixed-size array so that a masked index
    /// needs no bounds check.
    state_table: Box<[u16; STATE_CELLS]>,
    /// One transform per byte value, zero past the alphabet.
    symbol_tt: Box<[SymbolTransform; 256]>,
}

/// The cells of the largest state table.
const STATE_CELLS: usize = 1 << MAX_TABLELOG;

impl CTable {
    /// `FSE_buildCTable`: builds the table of `norm[0..=max_symbol_value]`.
    ///
    /// # Errors
    ///
    /// A table log above [`MAX_TABLELOG`] or counts shorter than the
    /// alphabet.
    pub(crate) fn build(
        norm: &[i16],
        max_symbol_value: u32,
        table_log: u32,
    ) -> Result<Self, EntropyError> {
        if table_log > MAX_TABLELOG || max_symbol_value > MAX_SYMBOL_VALUE {
            return Err(EntropyError::TableLogTooLarge);
        }
        let max_sv1 = max_symbol_value as usize + 1;
        if norm.len() < max_sv1 {
            return Err(EntropyError::Generic);
        }
        let table_size = 1u32 << table_log;
        let table_mask = table_size - 1;
        let step = table_step(table_size);
        let mut cumul = vec![0u16; max_sv1 + 1];
        let mut table_symbol = vec![0u8; table_size as usize];
        let mut high_threshold = table_size - 1;

        // Symbol start positions; low-probability symbols fill the top.
        for u in 1..=max_sv1 {
            if norm[u - 1] == -1 {
                cumul[u] = cumul[u - 1] + 1;
                table_symbol[high_threshold as usize] = (u - 1) as u8;
                high_threshold = high_threshold.wrapping_sub(1);
            } else {
                cumul[u] = cumul[u - 1].wrapping_add(norm[u - 1] as u16);
            }
        }
        cumul[max_sv1] = (table_size + 1) as u16;

        // Spread the symbols.
        let mut position = 0u32;
        for (symbol, &freq) in norm[..max_sv1].iter().enumerate() {
            for _ in 0..freq.max(0) {
                table_symbol[position as usize] = symbol as u8;
                position = (position + step) & table_mask;
                while position > high_threshold {
                    position = (position + step) & table_mask;
                }
            }
        }

        // The state table, sorted by symbol.
        let mut state_table: Box<[u16; STATE_CELLS]> = Box::new([0u16; STATE_CELLS]);
        for (u, &symbol) in table_symbol.iter().enumerate() {
            let slot = &mut cumul[usize::from(symbol)];
            state_table[usize::from(*slot)] = (table_size as usize + u) as u16;
            *slot += 1;
        }

        // The symbol transformation table.
        let mut symbol_tt: Box<[SymbolTransform; 256]> =
            Box::new([SymbolTransform::default(); 256]);
        let mut total: u32 = 0;
        for (s, &count) in norm[..max_sv1].iter().enumerate() {
            match count {
                0 => {
                    symbol_tt[s].delta_nb_bits =
                        ((table_log + 1) << 16).wrapping_sub(1 << table_log);
                }
                -1 | 1 => {
                    symbol_tt[s].delta_nb_bits = (table_log << 16).wrapping_sub(1 << table_log);
                    symbol_tt[s].delta_find_state = total.wrapping_sub(1) as i32;
                    total += 1;
                }
                _ => {
                    let count = count as u32;
                    let max_bits_out = table_log - highbit32(count - 1);
                    let min_state_plus = count << max_bits_out;
                    symbol_tt[s].delta_nb_bits = (max_bits_out << 16).wrapping_sub(min_state_plus);
                    symbol_tt[s].delta_find_state = total.wrapping_sub(count) as i32;
                    total += count;
                }
            }
        }
        Ok(Self {
            table_log,
            state_table,
            symbol_tt,
        })
    }
}

/// `FSE_CState_t`.
#[derive(Clone, Copy, Debug, Default)]
struct CState {
    value: i64,
}

/// The tables an encoder loop reads, borrowed once so that they stay in
/// registers.
#[derive(Clone, Copy)]
struct Tables<'a> {
    log: u32,
    state_table: &'a [u16; STATE_CELLS],
    symbol_tt: &'a [SymbolTransform; 256],
}

impl CState {
    /// `FSE_initCState2`: the first symbol uses the smallest state.
    fn new(tables: Tables<'_>, symbol: u8) -> Self {
        let tt = tables.symbol_tt[usize::from(symbol)];
        let nb_bits_out = tt.delta_nb_bits.wrapping_add(1 << 15) >> 16;
        let value = i64::from((nb_bits_out << 16).wrapping_sub(tt.delta_nb_bits));
        let index = (value >> nb_bits_out) + i64::from(tt.delta_find_state);
        Self {
            value: i64::from(tables.state_table[index as usize & (STATE_CELLS - 1)]),
        }
    }

    /// `FSE_encodeSymbol`.
    #[inline(always)]
    fn encode(&mut self, bits: &mut WordWriter, tables: Tables<'_>, symbol: u8) {
        let tt = tables.symbol_tt[usize::from(symbol)];
        let nb_bits_out = ((self.value + i64::from(tt.delta_nb_bits)) >> 16) as u32;
        bits.add(self.value as u64 & ((1u64 << nb_bits_out) - 1), nb_bits_out);
        let index = (self.value >> nb_bits_out) + i64::from(tt.delta_find_state);
        self.value = i64::from(tables.state_table[index as usize & (STATE_CELLS - 1)]);
    }

    /// `FSE_flushCState`.
    fn flush(&self, bits: &mut WordWriter, tables: Tables<'_>) {
        bits.add(self.value as u64 & ((1u64 << tables.log) - 1), tables.log);
    }
}

/// `FSE_compress_usingCTable`: compresses `src` with `nb_states` (2 or 4)
/// interleaved states into a destination of `dst_capacity` bytes.
///
/// Returns `None` where the reference returns 0: an unsupported state
/// count, a source of at most `nb_states` symbols, or a stream that does not
/// fit.
pub(crate) fn compress_using_ctable(
    level: Level,
    src: &[u8],
    ct: &CTable,
    nb_states: usize,
    dst_capacity: usize,
) -> Option<Vec<u8>> {
    // The kernel is inlined into the dispatch target, so it is compiled with
    // the features of `level` (BMI2 shifts on x86-64-v3).
    dispatch!(level, _simd => compress_kernel(src, ct, nb_states, dst_capacity))
}

/// [`compress_using_ctable`] at the baseline level, for callers with no
/// level at hand and a few bytes to code (the Huffman weights), and the body
/// that [`compress_using_ctable`] compiles once per dispatch target.
#[inline(always)]
pub(crate) fn compress_kernel(
    src: &[u8],
    ct: &CTable,
    nb_states: usize,
    dst_capacity: usize,
) -> Option<Vec<u8>> {
    if src.len() <= nb_states {
        return None;
    }
    match nb_states {
        2 => compress_states::<2>(src, ct, dst_capacity),
        4 => compress_states::<4>(src, ct, dst_capacity),
        _ => None,
    }
}

/// [`compress_kernel`] with `STATES` (2 or 4) states, which the loop keeps in
/// registers: the states take the symbols in turn, from the last one back.
#[inline(always)]
fn compress_states<const STATES: usize>(
    src: &[u8],
    ct: &CTable,
    dst_capacity: usize,
) -> Option<Vec<u8>> {
    let tables = Tables {
        log: ct.table_log,
        state_table: &ct.state_table,
        symbol_tt: &ct.symbol_tt,
    };
    // No symbol takes more than `table_log` bits, and the stream ends with
    // the states and a mark. A destination smaller than that lets the writer
    // run out of room, where the reference fails too.
    let bound = (src.len() * tables.log as usize).div_ceil(8) + 2 * STATES * 2 + 2;
    let mut bits = WordWriter::with_capacity(bound.min(dst_capacity));
    let mut states = [CState::default(); STATES];
    let mut ip = src.len();
    let mut s = STATES - (src.len() % STATES);
    for _ in 0..STATES {
        ip -= 1;
        states[s % STATES] = CState::new(tables, src[ip]);
        s += 1;
    }
    for _ in 0..src.len() % STATES {
        ip -= 1;
        states[s % STATES].encode(&mut bits, tables, src[ip]);
        s += 1;
    }
    debug_assert_eq!(s % STATES, 0);
    if !bits.flush() {
        return None;
    }
    debug_assert_eq!(ip % STATES, 0);
    // Four symbols of at most 12 bits between flushes: states 0..4 in turn
    // for four states, 0, 1, 0, 1 for two.
    let (head, chunks) = src[..ip].as_rchunks::<4>();
    for chunk in chunks.iter().rev() {
        states[0].encode(&mut bits, tables, chunk[3]);
        states[1].encode(&mut bits, tables, chunk[2]);
        states[2 % STATES].encode(&mut bits, tables, chunk[1]);
        states[3 % STATES].encode(&mut bits, tables, chunk[0]);
        if !bits.flush() {
            return None;
        }
    }
    // Two states leave a pair of symbols in front of the quads.
    for (lane, &symbol) in head.iter().rev().enumerate() {
        states[lane % STATES].encode(&mut bits, tables, symbol);
    }
    for state in &states {
        state.flush(&mut bits, tables);
    }
    bits.close(dst_capacity)
}

/// `FSE_writeNCount`: serialises `norm[0..=max_symbol_value]` as a count
/// header.
///
/// # Errors
///
/// A table log outside `5..=12`, or counts that do not sum to a table.
pub(crate) fn write_ncount(
    norm: &[i16],
    max_symbol_value: u32,
    table_log: u32,
) -> Result<Vec<u8>, EntropyError> {
    if table_log > MAX_TABLELOG {
        return Err(EntropyError::TableLogTooLarge);
    }
    if table_log < MIN_TABLELOG {
        return Err(EntropyError::Generic);
    }
    let mut out = Vec::with_capacity(NCOUNT_BOUND);
    let table_size = 1i32 << table_log;
    let alphabet_size = max_symbol_value + 1;
    let mut bit_stream: u32 = 0;
    let mut bit_count: i32 = 0;
    let mut symbol: u32 = 0;
    let mut previous_is_0 = false;

    // Table size.
    bit_stream = bit_stream.wrapping_add((table_log - MIN_TABLELOG) << bit_count);
    bit_count += 4;

    let mut remaining = table_size + 1;
    let mut threshold = table_size;
    let mut nb_bits = table_log as i32 + 1;

    while symbol < alphabet_size && remaining > 1 {
        if previous_is_0 {
            let mut start = symbol;
            while symbol < alphabet_size && norm[symbol as usize] == 0 {
                symbol += 1;
            }
            if symbol == alphabet_size {
                break;
            }
            while symbol >= start + 24 {
                start += 24;
                bit_stream = bit_stream.wrapping_add(0xFFFFu32 << bit_count);
                out.push(bit_stream as u8);
                out.push((bit_stream >> 8) as u8);
                bit_stream >>= 16;
            }
            while symbol >= start + 3 {
                start += 3;
                bit_stream = bit_stream.wrapping_add(3u32 << bit_count);
                bit_count += 2;
            }
            bit_stream = bit_stream.wrapping_add((symbol - start) << bit_count);
            bit_count += 2;
            if bit_count > 16 {
                out.push(bit_stream as u8);
                out.push((bit_stream >> 8) as u8);
                bit_stream >>= 16;
                bit_count -= 16;
            }
        }
        {
            let mut count = i32::from(norm[symbol as usize]);
            symbol += 1;
            let max = (2 * threshold - 1) - remaining;
            remaining -= count.abs();
            count += 1;
            if count >= threshold {
                count += max;
            }
            bit_stream = bit_stream.wrapping_add((count as u32) << bit_count);
            bit_count += nb_bits;
            bit_count -= i32::from(count < max);
            previous_is_0 = count == 1;
            if remaining < 1 {
                return Err(EntropyError::Generic);
            }
            while remaining < threshold {
                nb_bits -= 1;
                threshold >>= 1;
            }
        }
        if bit_count > 16 {
            out.push(bit_stream as u8);
            out.push((bit_stream >> 8) as u8);
            bit_stream >>= 16;
            bit_count -= 16;
        }
    }

    if remaining != 1 {
        return Err(EntropyError::Generic);
    }

    // Flush the remaining bits.
    out.push(bit_stream as u8);
    out.push((bit_stream >> 8) as u8);
    let len = out.len() - 2 + ((bit_count + 7) / 8) as usize;
    out.truncate(len);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Backend;

    fn level() -> Level {
        Backend::default().0
    }

    fn histogram(src: &[u8]) -> (Vec<u32>, u32) {
        let mut count = vec![0u32; 256];
        for &b in src {
            count[usize::from(b)] += 1;
        }
        let max = count.iter().rposition(|&c| c != 0).unwrap_or(0) as u32;
        (count, max)
    }

    #[test]
    fn bounds_follow_the_macros() {
        assert_eq!(block_bound(0), 12);
        assert_eq!(block_bound(256), 256 + 2 + 12);
        assert_eq!(compress_bound(0), 524);
    }

    #[test]
    fn optimal_table_log_matches_the_reference_on_tiny_and_large_sources() {
        // A two-byte source wraps the unsigned max-bits computation.
        assert_eq!(optimal_table_log(DEFAULT_TABLELOG, 2, 1), 11);
        assert_eq!(optimal_table_log(DEFAULT_TABLELOG, 100, 3), 5);
        assert_eq!(optimal_table_log(DEFAULT_TABLELOG, 1 << 20, 255), 11);
        assert_eq!(optimal_table_log(0, 1 << 20, 255), 11);
        assert_eq!(optimal_table_log(14, 1 << 20, 255), 12);
        assert_eq!(optimal_table_log_internal(6, 1000, 12, 1), 6);
    }

    #[test]
    fn normalised_counts_sum_to_the_table_and_round_trip_through_ncount() {
        let src: Vec<u8> = (0..4000u32).map(|i| ((i * i) % 23) as u8).collect();
        let (count, max) = histogram(&src);
        let log = optimal_table_log(DEFAULT_TABLELOG, src.len(), max);
        let mut norm = vec![0i16; 256];
        assert_eq!(
            normalize_count(&mut norm, log, &count, src.len(), max, true),
            Ok(log)
        );
        let sum: i32 = norm[..=max as usize]
            .iter()
            .map(|&n| i32::from(n.abs()))
            .sum();
        assert_eq!(sum, 1 << log);
        let header = write_ncount(&norm, max, log).unwrap();
        assert!(!header.is_empty() && header.len() <= NCOUNT_BOUND);
        assert_eq!(header[0] & 15, (log - MIN_TABLELOG) as u8);
    }

    #[test]
    fn normalisation_rejects_bad_table_logs_and_reports_constants() {
        let (count, max) = histogram(b"aaaabbbbcc");
        let mut norm = vec![0i16; 256];
        assert_eq!(
            normalize_count(&mut norm, 4, &count, 10, max, false),
            Err(EntropyError::Generic)
        );
        assert_eq!(
            normalize_count(&mut norm, 13, &count, 10, max, false),
            Err(EntropyError::TableLogTooLarge)
        );
        // A log too small for the alphabet.
        let wide = (0..=255u8).collect::<Vec<_>>();
        let (count, max) = histogram(&wide);
        assert_eq!(
            normalize_count(&mut norm, 5, &count, 256, max, false),
            Err(EntropyError::Generic)
        );
        let (count, max) = histogram(b"zzzzzzzz");
        assert_eq!(normalize_count(&mut norm, 6, &count, 8, max, true), Ok(0));
        // Table log 0 means the default.
        let (count, max) = histogram(&[1u8, 2, 3, 1, 2, 3, 1, 1]);
        assert_eq!(
            normalize_count(&mut norm, 0, &count, 8, max, true).map(|log| log >= 5),
            Ok(true)
        );
    }

    #[test]
    fn the_secondary_normalisation_handles_flat_distributions() {
        // Many symbols with near-equal counts push the primary method past its
        // rounding budget; the result must still sum to the table.
        for symbols in [60usize, 200, 255] {
            let mut src = Vec::new();
            for s in 0..symbols {
                src.extend(core::iter::repeat_n(s as u8, 3 + s % 4));
            }
            let (count, max) = histogram(&src);
            let log = optimal_table_log(DEFAULT_TABLELOG, src.len(), max);
            let mut norm = vec![0i16; 256];
            normalize_count(&mut norm, log, &count, src.len(), max, false).unwrap();
            let sum: i32 = norm[..=max as usize]
                .iter()
                .map(|&n| i32::from(n.abs()))
                .sum();
            assert_eq!(sum, 1 << log, "{symbols} symbols");
        }
    }

    #[test]
    fn m2_normalisation_distributes_rounding_to_the_largest_or_by_weight() {
        let count = [1000u32, 3, 3, 3, 2, 1, 1, 1, 1];
        let total: usize = count.iter().map(|&c| c as usize).sum();
        let mut norm = vec![0i16; 9];
        normalize_m2(&mut norm, 6, &count, total, 8, -1).unwrap();
        let sum: i32 = norm.iter().map(|&n| i32::from(n.abs())).sum();
        assert_eq!(sum, 64);
        // Everything poor: the remainder goes to the most frequent symbol.
        let count = [2u32, 3, 2, 2];
        let mut norm = vec![0i16; 4];
        normalize_m2(&mut norm, 5, &count, 9, 3, 1).unwrap();
        assert_eq!(norm.iter().map(|&n| i32::from(n)).sum::<i32>(), 32);
        // Total exhausted by the low thresholds.
        let count = [1u32; 6];
        let mut norm = vec![0i16; 6];
        normalize_m2(&mut norm, 5, &count, 6, 5, 1).unwrap();
        assert_eq!(norm.iter().map(|&n| i32::from(n)).sum::<i32>(), 32);
    }

    #[test]
    fn ctables_encode_with_two_and_four_states() {
        let src: Vec<u8> = (0..3000u32).map(|i| ((i * 7 + i / 3) % 11) as u8).collect();
        let (count, max) = histogram(&src);
        let log = optimal_table_log(DEFAULT_TABLELOG, src.len(), max);
        let mut norm = vec![0i16; 256];
        normalize_count(&mut norm, log, &count, src.len(), max, true).unwrap();
        let ct = CTable::build(&norm, max, log).unwrap();
        assert_eq!(ct.table_log, log);
        for states in [2, 4] {
            let out = compress_using_ctable(level(), &src, &ct, states, compress_bound(src.len()))
                .unwrap();
            assert!(out.len() < src.len());
            assert_ne!(*out.last().unwrap(), 0, "the end mark is in the last byte");
        }
        assert!(compress_using_ctable(level(), &src, &ct, 3, 1 << 16).is_none());
        // At most `states` symbols are never compressed.
        assert!(compress_using_ctable(level(), &src[..2], &ct, 2, 1 << 16).is_none());
        assert!(compress_using_ctable(level(), &src[..3], &ct, 2, 1 << 16).is_some());
        assert!(compress_using_ctable(level(), &src[..4], &ct, 4, 1 << 16).is_none());
        // A destination that is too small.
        assert!(compress_using_ctable(level(), &src, &ct, 2, 16).is_none());
    }

    #[test]
    fn ctable_construction_validates_its_arguments() {
        let norm = [16i16, 16];
        assert!(CTable::build(&norm, 1, 5).is_ok());
        assert_eq!(
            CTable::build(&norm, 1, 13).unwrap_err(),
            EntropyError::TableLogTooLarge
        );
        assert_eq!(
            CTable::build(&norm, 300, 5).unwrap_err(),
            EntropyError::TableLogTooLarge
        );
        assert_eq!(
            CTable::build(&norm, 2, 5).unwrap_err(),
            EntropyError::Generic
        );
        // Low-probability and absent symbols.
        let norm = [-1i16, 0, 28, 3];
        let ct = CTable::build(&norm, 3, 5).unwrap();
        assert_eq!(ct.table_log, 5);
    }

    #[test]
    fn ncount_headers_reject_inconsistent_counts() {
        let norm = [16i16, 16];
        assert_eq!(
            write_ncount(&norm, 1, 13),
            Err(EntropyError::TableLogTooLarge)
        );
        assert_eq!(write_ncount(&norm, 1, 4), Err(EntropyError::Generic));
        assert_eq!(write_ncount(&[20i16, 20], 1, 5), Err(EntropyError::Generic));
        assert_eq!(write_ncount(&[8i16, 8], 1, 5), Err(EntropyError::Generic));
        assert_eq!(write_ncount(&norm, 1, 5), Ok(alloc::vec![0x10, 0x3f]));
        // Runs of zero counts use the repeat codes.
        let mut sparse = vec![0i16; 100];
        sparse[0] = 16;
        sparse[99] = 16;
        let header = write_ncount(&sparse, 99, 5).unwrap();
        assert!(header.len() > 2);
    }

    #[test]
    fn the_encoder_writes_the_same_bytes_on_every_level() {
        let src: Vec<u8> = (0..if cfg!(miri) { 300 } else { 5001u32 })
            .map(|i| ((i * 7 + i / 3) % 11) as u8)
            .collect();
        let mut count = vec![0u32; 256];
        for &byte in &src {
            count[usize::from(byte)] += 1;
        }
        let max = 10;
        let log = optimal_table_log(DEFAULT_TABLELOG, src.len(), max);
        let mut norm = vec![0i16; max as usize + 1];
        normalize_count(&mut norm, log, &count, src.len(), max, true).unwrap();
        let ct = CTable::build(&norm, max, log).unwrap();
        for states in [2, 4] {
            let bound = compress_bound(src.len());
            let expected = compress_kernel(&src, &ct, states, bound).unwrap();
            for backend in Backend::available() {
                assert_eq!(
                    compress_using_ctable(backend.0, &src, &ct, states, bound).as_ref(),
                    Some(&expected),
                    "{backend}, {states} states"
                );
            }
        }
    }
}
