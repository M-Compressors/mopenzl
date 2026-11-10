//! The optimal parser of lz4hc.c (`LZ4HC_compress_optimal`, levels 10 to 12),
//! for a fresh stream, `notLimited` output and no `favorDecSpeed`.

use alloc::vec;
use alloc::vec::Vec;

use super::block::{FIELD_MAX, LAST_LITERALS, MF_LIMIT, MIN_MATCH, push_last_literals};
use super::hc::{Hc, LevelParams, Match};

/// `LZ4_OPT_NUM`.
pub(super) const OPT_NUM: usize = 1 << 12;
/// `TRAILING_LITERALS`.
const TRAILING_LITERALS: usize = 3;

/// `LZ4HC_optimal_t`.
#[derive(Copy, Clone, Default)]
struct Node {
    price: i32,
    off: i32,
    mlen: i32,
    litlen: i32,
}

/// `LZ4HC_literalsPrice`: the cost in bytes of `litlen` literals.
const fn literals_price(litlen: i32) -> i32 {
    let mut price = litlen;
    if litlen >= FIELD_MAX as i32 {
        price += 1 + (litlen - FIELD_MAX as i32) / 255;
    }
    price
}

/// `LZ4HC_sequencePrice`.
const fn sequence_price(litlen: i32, mlen: i32) -> i32 {
    let mut price = 1 + 2;
    price += literals_price(litlen);
    if mlen >= (FIELD_MAX + MIN_MATCH) as i32 {
        price += 1 + (mlen - (FIELD_MAX + MIN_MATCH) as i32) / 255;
    }
    price
}

/// `LZ4HC_FindLongerMatch`.
fn find_longer_match(
    hc: &mut Hc<'_>,
    ip: usize,
    high: usize,
    min_len: i32,
    searches: i32,
) -> Match {
    let md = hc.insert_and_get_wider_match::<true, true>(ip, ip, high, min_len, searches);
    if md.len <= min_len {
        return Match::default();
    }
    md
}

/// `LZ4HC_compress_optimal` and its sequence encoder.
pub(super) fn compress(hc: &mut Hc<'_>, out: &mut Vec<u8>, params: LevelParams, full_update: bool) {
    let src = hc.src;
    let n = src.len();
    let searches = params.searches;
    let sufficient_len = params.target_length.min(OPT_NUM - 1);
    let mut opt = vec![Node::default(); OPT_NUM + TRAILING_LITERALS];
    let mut ip = 0usize;
    let mut anchor = 0usize;
    // `iend - MFLIMIT` and `iend - LASTLITERALS` as positions; a block below
    // `MFLIMIT` bytes never enters the loop.
    let mflimit = n.checked_sub(MF_LIMIT);
    let matchlimit = n.saturating_sub(LAST_LITERALS);

    while mflimit.is_some_and(|limit| ip <= limit) {
        let mflimit = n - MF_LIMIT;
        let llen = (ip - anchor) as i32;
        let first_match = find_longer_match(hc, ip, matchlimit, MIN_MATCH as i32 - 1, searches);
        if first_match.len == 0 {
            ip += 1;
            continue;
        }
        if first_match.len as usize > sufficient_len {
            encode(
                out,
                src,
                &mut ip,
                &mut anchor,
                first_match.len,
                first_match.off,
            );
            continue;
        }
        // Prices of the first positions (literals).
        for (r_pos, node) in opt.iter_mut().take(MIN_MATCH).enumerate() {
            *node = Node {
                mlen: 1,
                off: 0,
                litlen: llen + r_pos as i32,
                price: literals_price(llen + r_pos as i32),
            };
        }
        // Prices using the initial match.
        let match_ml = first_match.len;
        for mlen in MIN_MATCH as i32..=match_ml {
            opt[mlen as usize] = Node {
                mlen,
                off: first_match.off,
                litlen: llen,
                price: sequence_price(llen, mlen),
            };
        }
        let mut last_match_pos = first_match.len as usize;
        for add_lit in 1..=TRAILING_LITERALS {
            opt[last_match_pos + add_lit] = Node {
                mlen: 1,
                off: 0,
                litlen: add_lit as i32,
                price: opt[last_match_pos].price + literals_price(add_lit as i32),
            };
        }

        // Check further positions.
        let mut cur = 1usize;
        let mut immediate: Option<(i32, i32)> = None;
        while cur < last_match_pos {
            let cur_ptr = ip + cur;
            if cur_ptr > mflimit {
                break;
            }
            let skip = if full_update {
                opt[cur + 1].price <= opt[cur].price
                    && opt[cur + MIN_MATCH].price < opt[cur].price + 3
            } else {
                opt[cur + 1].price <= opt[cur].price
            };
            if skip {
                cur += 1;
                continue;
            }
            let new_match = if full_update {
                find_longer_match(hc, cur_ptr, matchlimit, MIN_MATCH as i32 - 1, searches)
            } else {
                find_longer_match(
                    hc,
                    cur_ptr,
                    matchlimit,
                    (last_match_pos - cur) as i32,
                    searches,
                )
            };
            if new_match.len == 0 {
                cur += 1;
                continue;
            }
            if new_match.len as usize > sufficient_len || new_match.len as usize + cur >= OPT_NUM {
                immediate = Some((new_match.len, new_match.off));
                last_match_pos = cur + 1;
                break;
            }
            // Before the match: prices with literals at the beginning.
            let base_litlen = opt[cur].litlen;
            for litlen in 1..MIN_MATCH as i32 {
                let price = opt[cur].price - literals_price(base_litlen)
                    + literals_price(base_litlen + litlen);
                let pos = cur + litlen as usize;
                if price < opt[pos].price {
                    opt[pos] = Node {
                        mlen: 1,
                        off: 0,
                        litlen: base_litlen + litlen,
                        price,
                    };
                }
            }
            // Prices using the match at `cur`.
            let match_ml = new_match.len;
            for ml in MIN_MATCH as i32..=match_ml {
                let pos = cur + ml as usize;
                let (ll, price) = if opt[cur].mlen == 1 {
                    let ll = opt[cur].litlen;
                    let before = if cur as i32 > ll {
                        opt[cur - ll as usize].price
                    } else {
                        0
                    };
                    (ll, before + sequence_price(ll, ml))
                } else {
                    (0, opt[cur].price + sequence_price(0, ml))
                };
                if pos > last_match_pos + TRAILING_LITERALS || price <= opt[pos].price {
                    if ml == match_ml && last_match_pos < pos {
                        last_match_pos = pos;
                    }
                    opt[pos] = Node {
                        mlen: ml,
                        off: new_match.off,
                        litlen: ll,
                        price,
                    };
                }
            }
            // Complete the following positions with literals.
            for add_lit in 1..=TRAILING_LITERALS {
                opt[last_match_pos + add_lit] = Node {
                    mlen: 1,
                    off: 0,
                    litlen: add_lit as i32,
                    price: opt[last_match_pos].price + literals_price(add_lit as i32),
                };
            }
            cur += 1;
        }

        let (best_mlen, best_off);
        if let Some((len, off)) = immediate {
            best_mlen = len;
            best_off = off;
        } else {
            best_mlen = opt[last_match_pos].mlen;
            best_off = opt[last_match_pos].off;
            cur = last_match_pos - best_mlen as usize;
        }

        // Reverse traversal, from the end to the beginning.
        let mut candidate_pos = cur;
        let mut selected_match_length = best_mlen;
        let mut selected_offset = best_off;
        loop {
            let next_match_length = opt[candidate_pos].mlen;
            let next_offset = opt[candidate_pos].off;
            opt[candidate_pos].mlen = selected_match_length;
            opt[candidate_pos].off = selected_offset;
            selected_match_length = next_match_length;
            selected_offset = next_offset;
            if next_match_length as usize > candidate_pos {
                break;
            }
            candidate_pos -= next_match_length as usize;
        }

        // Encode all recorded sequences in order.
        let mut r_pos = 0usize;
        while r_pos < last_match_pos {
            let ml = opt[r_pos].mlen;
            let offset = opt[r_pos].off;
            if ml == 1 {
                ip += 1;
                r_pos += 1;
                continue;
            }
            r_pos += ml as usize;
            encode(out, src, &mut ip, &mut anchor, ml, offset);
        }
    }
    push_last_literals(out, src, anchor);
}

/// `LZ4HC_encodeSequence` with `notLimited`.
fn encode(out: &mut Vec<u8>, src: &[u8], ip: &mut usize, anchor: &mut usize, ml: i32, off: i32) {
    super::hc::encode_sequence(out, src, ip, anchor, ml, off);
}
