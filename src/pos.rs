//! Position representation.
//!
//! Convention: a `Position` always represents the state *from the point of
//! view of the side to move*. `white()` is always the mover's stones,
//! `black()` is always the opponent's stones — regardless of which physical
//! color is actually on move. This is the "color normalization" from the
//! design doc: values computed for a `Position` are always "for the side to
//! move", and every stored/looked-up state has this canonical orientation.

pub const FULL_MASK: u32 = (1 << 24) - 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Position(pub u64);

impl Position {
    #[inline]
    pub fn new(white: u32, black: u32) -> Self {
        debug_assert_eq!(white & !FULL_MASK, 0);
        debug_assert_eq!(black & !FULL_MASK, 0);
        debug_assert_eq!(white & black, 0, "white and black overlap");
        Position((white as u64) | ((black as u64) << 24))
    }

    #[inline]
    pub fn white(&self) -> u32 {
        (self.0 & FULL_MASK as u64) as u32
    }

    #[inline]
    pub fn black(&self) -> u32 {
        ((self.0 >> 24) & FULL_MASK as u64) as u32
    }

    #[inline]
    pub fn occupied(&self) -> u32 {
        self.white() | self.black()
    }

    #[inline]
    pub fn empty(&self) -> u32 {
        !self.occupied() & FULL_MASK
    }

    #[inline]
    pub fn white_count(&self) -> u32 {
        self.white().count_ones()
    }

    #[inline]
    pub fn black_count(&self) -> u32 {
        self.black().count_ones()
    }

    /// Swap the two colors (used when we need the "physical" flip rather
    /// than a move-induced perspective flip, e.g. for I/O).
    #[inline]
    pub fn swap_colors(&self) -> Position {
        Position::new(self.black(), self.white())
    }

    /// The side to move has no legal move: either blocked (all stones
    /// immobile, only relevant with >=4 stones since 3 stones can always
    /// jump to any empty point) — this is one of the two terminal-loss
    /// conditions.
    pub fn is_blocked(&self) -> bool {
        if self.white_count() == 0 {
            return true;
        }
        if self.white_count() == 3 {
            // can jump anywhere; blocked only if board is full (impossible
            // since black has stones and total <= 18 < 24, so never blocked)
            return self.empty() == 0;
        }
        let empty = self.empty();
        crate::board::ADJ
            .iter()
            .enumerate()
            .all(|(p, adj)| self.white() & (1 << p) == 0 || adj & empty == 0)
    }

    /// True if the side to move has fewer than three stones (a loss).
    pub fn white_has_too_few(&self) -> bool {
        self.white_count() < 3
    }

    pub fn is_mill_at(bits: u32, p: usize) -> bool {
        crate::board::POINT_MILLS[p]
            .iter()
            .any(|m| *m != 0 && bits & m == *m)
    }
}

impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for p in 0..24 {
            let c = if self.white() & (1 << p) != 0 {
                'W'
            } else if self.black() & (1 << p) != 0 {
                'B'
            } else {
                '.'
            };
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

/// Iterate over set bit positions of a 32-bit mask.
#[inline]
pub fn bits(mask: u32) -> impl Iterator<Item = usize> {
    let mut m = mask;
    std::iter::from_fn(move || {
        if m == 0 {
            None
        } else {
            let p = m.trailing_zeros() as usize;
            m &= m - 1;
            Some(p)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessors_roundtrip() {
        let p = Position::new(0b101, 0b010);
        assert_eq!(p.white(), 0b101);
        assert_eq!(p.black(), 0b010);
    }

    #[test]
    fn swap_colors_roundtrip() {
        let p = Position::new(0b101, 0b010);
        assert_eq!(p.swap_colors().swap_colors(), p);
    }

    #[test]
    fn bits_iterates_all_set() {
        let mask = 0b1011u32;
        let v: Vec<usize> = bits(mask).collect();
        assert_eq!(v, vec![0, 1, 3]);
    }

    #[test]
    fn blocked_position_from_figure_3() {
        // Not constructing the exact figure, but a hand-built fully
        // surrounded single stone should be blocked.
        // Point 0 (a7) has neighbors 1 (d7) and 7 (a4).
        let white = 1u32; // stone at point 0
        let black = (1 << 1) | (1 << 7); // neighbors occupied
        let p = Position::new(white, black);
        assert!(p.is_blocked());
    }

    #[test]
    fn not_blocked_with_empty_neighbor() {
        let white = 1u32;
        let black = 1 << 1; // only one neighbor occupied
        let p = Position::new(white, black);
        assert!(!p.is_blocked());
    }

    #[test]
    fn three_stones_never_blocked_unless_board_full() {
        let white = 0b111u32;
        let black = FULL_MASK & !white & !(1 << 23); // one empty square left
        let p = Position::new(white, black);
        assert!(!p.is_blocked());
    }
}
