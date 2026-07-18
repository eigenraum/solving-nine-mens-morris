"""Movement-phase move generation: slide/jump, mill closure, capture, terminals.

Independent Python port of the movement-phase half of `src/movegen.rs` /
`readme-database.md` §6. A `Position` here is a plain `(mover, opp)` pair of
24-bit ints -- always "side to move" normalized, exactly the database convention
(see `db.py`). Every successor returned is already perspective-flipped: its
`mover` is the *old opponent* (with the captured stone removed, if the move
closed a mill) and its `opp` is the *old mover*'s updated stone set
(`readme-database.md` §6 / `pos.rs`'s `Position::new(white, black)` convention,
where `white` = mover).
"""

from __future__ import annotations

from dataclasses import dataclass

from .board import ADJ, FULL_MASK, N, POINT_MILLS


def is_mill_at(mask: int, p: int) -> bool:
    """True iff point `p` is occupied in `mask` and completes one of its two
    mills (i.e. `mask` fully covers at least one of `POINT_MILLS[p]`)."""
    for mill in POINT_MILLS[p]:
        if mask & mill == mill:
            return True
    return False


def removable_stones(opponent: int) -> int:
    """Opponent stones that may legally be captured: not-in-mill, unless all
    opponent stones are in mills, in which case any of them."""
    not_in_mill = 0
    m = opponent
    while m:
        p = (m & -m).bit_length() - 1
        m &= m - 1
        if not is_mill_at(opponent, p):
            not_in_mill |= 1 << p
    return not_in_mill if not_in_mill else opponent


def _resolve_mill(mover: int, opponent: int, dest: int) -> list[int]:
    """New opponent masks after a stone lands on `dest`: a single unchanged
    mask if no mill closed, or one mask per legal capture choice."""
    if not is_mill_at(mover, dest):
        return [opponent]
    removable = removable_stones(opponent)
    out = []
    m = removable
    while m:
        s = (m & -m).bit_length() - 1
        m &= m - 1
        out.append(opponent & ~(1 << s))
    return out


@dataclass(frozen=True)
class Move:
    src: int
    dst: int
    captured: int | None  # point removed, or None if quiet
    successor_mover: int  # = old opponent, minus the captured stone if any
    successor_opp: int  # = old mover, updated


def moves_movement(mover: int, opponent: int) -> list[Move]:
    """All legal movement-phase moves from (mover, opponent).

    Mover must have >= 3 stones (fewer is terminal, handled by the caller --
    see `terminal_value`). With exactly 3 stones the mover may jump to any
    empty point instead of only sliding to an adjacent one.
    """
    empty = ~(mover | opponent) & FULL_MASK
    jump = bin(mover).count("1") == 3
    out: list[Move] = []
    m = mover
    while m:
        src = (m & -m).bit_length() - 1
        m &= m - 1
        dests = empty if jump else (int(ADJ[src]) & empty)
        d = dests
        while d:
            dst = (d & -d).bit_length() - 1
            d &= d - 1
            new_mover = (mover & ~(1 << src)) | (1 << dst)
            for new_opp in _resolve_mill(new_mover, opponent, dst):
                captured = None
                if new_opp != opponent:
                    captured = (opponent & ~new_opp).bit_length() - 1
                out.append(Move(src, dst, captured, new_opp, new_mover))
    return out


def is_blocked(mover: int, opponent: int) -> bool:
    """True iff the side to move has no legal movement-phase move (only
    possible with >= 4 stones: a 3-stone side can always jump to some empty
    point, since at most 18 of 24 points can be occupied)."""
    return len(moves_movement(mover, opponent)) == 0


def terminal_class(mover: int, opponent: int) -> int | None:
    """WDL class (from `db.py`: -1/0/1) if this position is an immediate
    terminal for the side to move, else None. Two terminal rules
    (`readme-database.md` §6): fewer than 3 stones, or no legal move.
    """
    from .db import LOSS

    if bin(mover).count("1") < 3:
        return LOSS
    if is_blocked(mover, opponent):
        return LOSS
    return None
