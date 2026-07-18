"""The 16-element board automorphism group and canonicalization.

Independent Python port of `src/symmetry.rs` / `readme-database.md` §3. Point
``p = ring*8 + i`` maps under symmetry ``(a, b, s)`` to ``ring'*8 + i'`` where
``i' = (a*i + b) mod 8`` and ``ring' = 2-ring if s else ring``, for
``a in {1, -1}``, ``b in {0, 2, 4, 6}``, ``s in {0, 1}`` (16 combinations, generated
in that nested order so `PERMS` indices line up with the Rust implementation for
easy cross-debugging — not required for correctness, since every consumer only
relies on "some enumeration of all 16", never on a specific index's meaning).
"""

from __future__ import annotations

import numpy as np

from .board import N

N_SYMS = 16


def _sym_map(a: int, b: int, s: int) -> np.ndarray:
    perm = np.zeros(N, dtype=np.int64)
    for p in range(N):
        ring, i = divmod(p, 8)
        i2 = (a * i + b) % 8
        ring2 = (2 - ring) if s else ring
        perm[p] = ring2 * 8 + i2
    return perm


def _build_perms() -> np.ndarray:
    perms = []
    for a in (1, -1):
        for b in (0, 2, 4, 6):
            for s in (0, 1):
                perms.append(_sym_map(a, b, s))
    out = np.stack(perms, axis=0)
    assert out.shape == (N_SYMS, N)
    return out


PERMS = _build_perms()  # PERMS[k][p] = image of point p under symmetry k


def apply(k: int, mask) -> int:
    """Apply symmetry k to a single 24-bit point-set mask (scalar, Python int)."""
    out = 0
    m = int(mask)
    while m:
        p = (m & -m).bit_length() - 1
        m &= m - 1
        out |= 1 << int(PERMS[k][p])
    return out


def apply_batch(k: int, masks: np.ndarray) -> np.ndarray:
    """Apply symmetry k to an array of uint32 masks, vectorized over `masks`."""
    masks = np.asarray(masks, dtype=np.uint32)
    out = np.zeros(masks.shape, dtype=np.uint32)
    for p in range(N):
        bit = (masks >> np.uint32(p)) & np.uint32(1)
        out |= bit.astype(np.uint32) << np.uint32(int(PERMS[k][p]))
    return out


def apply_pos(k: int, mover: int, opp: int) -> tuple[int, int]:
    return apply(k, mover), apply(k, opp)


def apply_pos_batch(
    k: int, movers: np.ndarray, opps: np.ndarray
) -> tuple[np.ndarray, np.ndarray]:
    return apply_batch(k, movers), apply_batch(k, opps)


def canonicalize(mover: int, opp: int) -> tuple[int, int, int]:
    """Return (canonical_mover, canonical_opp, sym_index).

    Canonical form minimizes (mover, opp) lexicographically (mover primary) over
    all 16 symmetry images — matching the indexing scheme, which ranks the
    canonical white (mover) set first (`readme-database.md` §3).
    """
    best_m, best_o = apply_pos(0, mover, opp)
    best_key = (best_m, best_o)
    best_sym = 0
    for k in range(1, N_SYMS):
        m, o = apply_pos(k, mover, opp)
        key = (m, o)
        if key < best_key:
            best_m, best_o, best_key, best_sym = m, o, key, k
    return best_m, best_o, best_sym


def canonicalize_batch(
    movers: np.ndarray, opps: np.ndarray
) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Vectorized canonicalize over arrays of positions.

    Returns (canonical_movers, canonical_opps, sym_index) as int64/uint32 arrays.
    """
    movers = np.asarray(movers, dtype=np.uint32)
    opps = np.asarray(opps, dtype=np.uint32)
    best_m, best_o = movers.copy(), opps.copy()
    best_sym = np.zeros(movers.shape, dtype=np.int64)
    # Compare as unsigned 64-bit (mover, opp) pairs packed for a total order that
    # matches Python tuple comparison: mover primary, opp secondary.
    best_key = best_m.astype(np.uint64) << np.uint64(32) | best_o.astype(np.uint64)
    for k in range(1, N_SYMS):
        m, o = apply_batch(k, movers), apply_batch(k, opps)
        key = m.astype(np.uint64) << np.uint64(32) | o.astype(np.uint64)
        better = key < best_key
        best_m = np.where(better, m, best_m)
        best_o = np.where(better, o, best_o)
        best_key = np.where(better, key, best_key)
        best_sym = np.where(better, k, best_sym)
    return best_m, best_o, best_sym


def is_canonical(mover: int, opp: int) -> bool:
    cm, co, _ = canonicalize(mover, opp)
    return cm == mover and co == opp


def is_canonical_batch(movers: np.ndarray, opps: np.ndarray) -> np.ndarray:
    cm, co, _ = canonicalize_batch(movers, opps)
    return (cm == np.asarray(movers, dtype=np.uint32)) & (
        co == np.asarray(opps, dtype=np.uint32)
    )


def stabilizer_size(mover: int, opp: int) -> int:
    count = 0
    for k in range(N_SYMS):
        m, o = apply_pos(k, mover, opp)
        if m == mover and o == opp:
            count += 1
    return count


def canonicalize_set(mask: int) -> int:
    """Canonical form of a bare point set (minimum image over all 16 symmetries)."""
    best = apply(0, mask)
    for k in range(1, N_SYMS):
        cand = apply(k, mask)
        if cand < best:
            best = cand
    return best
