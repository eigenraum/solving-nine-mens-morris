//! Board geometry: point numbering, adjacency, mills.
//!
//! 24 points on three concentric rings (0 = outer, 1 = middle, 2 = inner),
//! 8 points per ring. Within a ring, point `ring*8 + i` for `i` in `0..8`
//! runs clockwise starting at the top-left corner; even `i` are corners,
//! odd `i` are edge-midpoints. Odd-`i` midpoints carry the "spokes" that
//! connect adjacent rings at the same `i`.

/// Number of points on the board.
pub const N: usize = 24;

/// Number of mills.
pub const N_MILLS: usize = 16;

/// Adjacency bitmask for each point (bit `p` set iff `p` is adjacent).
pub const ADJ: [u32; N] = build_adj();

/// Bitmask for each of the 16 mills.
pub const MILLS: [u32; N_MILLS] = build_mills();

/// For each point, the (up to 2) mills it belongs to, as bitmasks.
/// Every point is in exactly 2 mills.
pub const POINT_MILLS: [[u32; 2]; N] = build_point_mills();

const fn ring_index(p: usize) -> (usize, usize) {
    (p / 8, p % 8)
}

const fn point(ring: usize, i: usize) -> usize {
    ring * 8 + (i % 8)
}

const fn build_adj() -> [u32; N] {
    let mut adj = [0u32; N];
    let mut ring = 0;
    while ring < 3 {
        let mut i = 0;
        while i < 8 {
            let p = point(ring, i);
            // ring neighbors (cycle of 8)
            let next = point(ring, (i + 1) % 8);
            let prev = point(ring, (i + 7) % 8);
            adj[p] |= 1 << next;
            adj[p] |= 1 << prev;
            i += 1;
        }
        ring += 1;
    }
    // spokes: odd i connects ring0-ring1 and ring1-ring2 at same i
    let mut i = 1;
    while i < 8 {
        let a = point(0, i);
        let b = point(1, i);
        let c = point(2, i);
        adj[a] |= 1 << b;
        adj[b] |= 1 << a;
        adj[b] |= 1 << c;
        adj[c] |= 1 << b;
        i += 2;
    }
    adj
}

const fn build_mills() -> [u32; N_MILLS] {
    let mut mills = [0u32; N_MILLS];
    let mut idx = 0;
    // ring mills: (i, i+1, i+2) for i = 0, 2, 4, 6, on each ring
    let mut ring = 0;
    while ring < 3 {
        let mut i = 0;
        while i < 8 {
            let a = point(ring, i);
            let b = point(ring, i + 1);
            let c = point(ring, i + 2);
            mills[idx] = (1 << a) | (1 << b) | (1 << c);
            idx += 1;
            i += 2;
        }
        ring += 1;
    }
    // spoke mills: outer-middle-inner at same odd i
    let mut i = 1;
    while i < 8 {
        let a = point(0, i);
        let b = point(1, i);
        let c = point(2, i);
        mills[idx] = (1 << a) | (1 << b) | (1 << c);
        idx += 1;
        i += 2;
    }
    mills
}

const fn build_point_mills() -> [[u32; 2]; N] {
    let mut pm = [[0u32; 2]; N];
    let mut p = 0;
    while p < N {
        let mut slot = 0;
        let mut m = 0;
        while m < N_MILLS {
            if MILLS[m] & (1 << p) != 0 {
                pm[p][slot] = MILLS[m];
                slot += 1;
            }
            m += 1;
        }
        p += 1;
    }
    pm
}

/// Convert a point index to standard "a1".."g7" notation (skipping the
/// unused center and non-board squares).
pub fn point_name(p: usize) -> String {
    let (ring, i) = ring_index(p);
    let h = 3 - ring as i32;
    let (col, row) = match i {
        0 => (4 - h, 4 + h),
        1 => (4, 4 + h),
        2 => (4 + h, 4 + h),
        3 => (4 + h, 4),
        4 => (4 + h, 4 - h),
        5 => (4, 4 - h),
        6 => (4 - h, 4 - h),
        7 => (4 - h, 4),
        _ => unreachable!(),
    };
    let letter = (b'a' + (col - 1) as u8) as char;
    format!("{letter}{row}")
}

/// Parse "a1".."g7" notation into a point index. Returns `None` for
/// squares that are not part of the board (e.g. "d4", the center).
pub fn parse_point(s: &str) -> Option<usize> {
    let mut chars = s.chars();
    let letter = chars.next()?;
    let rest: String = chars.collect();
    let row: i32 = rest.parse().ok()?;
    let col = (letter as i32) - ('a' as i32) + 1;
    if !(1..=7).contains(&col) || !(1..=7).contains(&row) {
        return None;
    }
    for p in 0..N {
        if point_name(p) == format!("{letter}{row}") {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degree_distribution() {
        // 12 corners with degree 2, 8 outer/inner midpoints with degree 3,
        // 4 middle midpoints with degree 4.
        let mut deg_count = std::collections::HashMap::new();
        for p in 0..N {
            let d = ADJ[p].count_ones();
            *deg_count.entry(d).or_insert(0) += 1;
        }
        assert_eq!(deg_count.get(&2), Some(&12));
        assert_eq!(deg_count.get(&3), Some(&8));
        assert_eq!(deg_count.get(&4), Some(&4));
    }

    #[test]
    fn edge_count() {
        let total: u32 = ADJ.iter().map(|m| m.count_ones()).sum();
        assert_eq!(total, 64); // 2 * 32 edges
    }

    #[test]
    fn adjacency_symmetric() {
        for p in 0..N {
            for q in 0..N {
                if ADJ[p] & (1 << q) != 0 {
                    assert!(ADJ[q] & (1 << p) != 0, "adjacency not symmetric for {p},{q}");
                }
            }
        }
    }

    #[test]
    fn no_self_adjacency() {
        for p in 0..N {
            assert_eq!(ADJ[p] & (1 << p), 0);
        }
    }

    #[test]
    fn mill_count_and_size() {
        assert_eq!(MILLS.len(), N_MILLS);
        for m in MILLS {
            assert_eq!(m.count_ones(), 3);
        }
    }

    #[test]
    fn every_point_in_exactly_two_mills() {
        for p in 0..N {
            let count = MILLS.iter().filter(|m| **m & (1 << p) != 0).count();
            assert_eq!(count, 2, "point {p} not in exactly 2 mills");
        }
    }

    #[test]
    fn mills_are_distinct() {
        let mut set = std::collections::HashSet::new();
        for m in MILLS {
            assert!(set.insert(m), "duplicate mill {m:x}");
        }
    }

    #[test]
    fn point_mills_matches_mills() {
        for p in 0..N {
            for m in POINT_MILLS[p] {
                assert!(MILLS.contains(&m));
                assert!(m & (1 << p) != 0);
            }
        }
    }

    #[test]
    fn point_name_roundtrip() {
        for p in 0..N {
            let name = point_name(p);
            assert_eq!(parse_point(&name), Some(p), "roundtrip failed for {p} ({name})");
        }
    }

    #[test]
    fn known_point_names() {
        // spot-check against the paper's coordinate scheme (Figure 1: a..g, 1..7)
        assert_eq!(point_name(0), "a7");
        assert_eq!(point_name(1), "d7");
        assert_eq!(point_name(2), "g7");
        assert_eq!(point_name(6), "a1");
        assert_eq!(point_name(9), "d6");
        assert_eq!(point_name(17), "d5");
    }

    #[test]
    fn mill_lines_are_geometrically_straight() {
        // spot check a couple of known mills by name
        let names: Vec<String> = (0..N).map(point_name).collect();
        let mill_names: Vec<Vec<&str>> = MILLS
            .iter()
            .map(|m| {
                (0..N)
                    .filter(|p| m & (1 << p) != 0)
                    .map(|p| names[p].as_str())
                    .collect()
            })
            .collect();
        let has = |a: &str, b: &str, c: &str| {
            mill_names.iter().any(|m| {
                let mut s: Vec<&str> = m.clone();
                s.sort();
                let mut want = vec![a, b, c];
                want.sort();
                s == want
            })
        };
        assert!(has("a7", "d7", "g7"));
        assert!(has("d7", "d6", "d5"));
        assert!(has("a1", "a4", "a7"));
    }
}
