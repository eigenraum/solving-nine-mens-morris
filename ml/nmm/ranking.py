"""Binomial table, combinatorial-number-system subset rank/unrank, canonical
white-set tables, and the position <-> (subspace, index) mapping.

Independent Python port of `src/index.rs` / `readme-database.md` §4. See that
module's docstring for the scheme; this file mirrors its algorithms exactly
(mask_rank / mask_unrank correspond 1:1 to `index.rs`'s functions of the same
name) but is written from scratch against the spec, not transliterated.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np

from . import symmetry
from .board import FULL_MASK, N, popcount

MIN_STONES = 3
MAX_STONES = 9
MAX_N = 25

CACHE_DIR = Path(__file__).resolve().parent.parent / "cache"

# Known-good sizes from readme-database.md §4.1 -- the hard N2 gate.
EXPECTED_CANONICAL_SIZES = {
    3: 158,
    4: 757,
    5: 2830,
    6: 8774,
    7: 22188,
    8: 46879,
    9: 82880,
}


def _build_binom() -> np.ndarray:
    c = np.zeros((MAX_N, MAX_N), dtype=np.int64)
    for n in range(MAX_N):
        c[n][0] = 1
        for k in range(1, n + 1):
            a = c[n - 1][k - 1]
            b = c[n - 1][k] if k < n else 0
            c[n][k] = a + b
    return c


BINOM = _build_binom()


def binom(n: int, k: int) -> int:
    if k < 0 or k > n or n < 0:
        return 0
    return int(BINOM[n][k])


# ---------------------------------------------------------------------------
# Canonical white-set tables
# ---------------------------------------------------------------------------


def _compute_canonical_sets() -> dict[int, np.ndarray]:
    """Enumerate, for each w in 3..=9, the sorted canonical (orbit-minimal)
    w-point subsets of the 24-point board. Single vectorized pass over all
    2**24 masks, mirroring `index.rs::build_index_tables`.
    """
    all_masks = np.arange(1 << N, dtype=np.uint32)
    counts = popcount(all_masks)
    result: dict[int, np.ndarray] = {}
    for w in range(MIN_STONES, MAX_STONES + 1):
        candidates = all_masks[counts == w]
        best = candidates.copy()
        for k in range(1, symmetry.N_SYMS):
            cand = symmetry.apply_batch(k, candidates)
            best = np.minimum(best, cand)
        canonical = np.sort(candidates[candidates == best])
        result[w] = canonical.astype(np.uint32)
    return result


_CANONICAL_SETS: dict[int, np.ndarray] | None = None


def canonical_white_sets(use_cache: bool = True) -> dict[int, np.ndarray]:
    """Return {w: sorted np.uint32[] of canonical w-point sets}, cached both
    in-process and on disk (`ml/cache/canonical_sets.npz`) since the full
    enumeration, while fast (a few seconds), is pure derived data.
    """
    global _CANONICAL_SETS
    if _CANONICAL_SETS is not None:
        return _CANONICAL_SETS

    cache_file = CACHE_DIR / "canonical_sets.npz"
    if use_cache and cache_file.exists():
        data = np.load(cache_file)
        sets = {w: data[f"w{w}"] for w in range(MIN_STONES, MAX_STONES + 1)}
    else:
        sets = _compute_canonical_sets()
        if use_cache:
            CACHE_DIR.mkdir(parents=True, exist_ok=True)
            np.savez(cache_file, **{f"w{w}": arr for w, arr in sets.items()})

    _CANONICAL_SETS = sets
    return sets


def n_canonical_white(w: int) -> int:
    return len(canonical_white_sets()[w])


def subspace_size(w: int, b: int) -> int:
    return n_canonical_white(w) * binom(24 - w, b)


# ---------------------------------------------------------------------------
# Combinatorial-number-system subset rank/unrank over a masked universe
# ---------------------------------------------------------------------------


def mask_rank(sub: int, universe: int) -> int:
    """Rank `sub` (subset of `universe`) among all `popcount(sub)`-subsets of
    `universe`, using the combinatorial number system (ascending compaction).
    """
    assert sub & ~universe == 0
    r = 0
    compact_idx = 0
    j = 0
    u = universe
    while u:
        p = (u & -u).bit_length() - 1
        u &= u - 1
        if sub & (1 << p):
            j += 1
            r += binom(compact_idx, j)
        compact_idx += 1
    return r


def mask_unrank(r: int, k: int, universe: int) -> int:
    """Inverse of `mask_rank`: the k-subset of `universe` with rank `r`."""
    avail = []
    u = universe
    while u:
        p = (u & -u).bit_length() - 1
        u &= u - 1
        avail.append(p)
    m = len(avail)
    compact = []
    upper = m
    j = k
    remaining = r
    while j >= 1:
        cand = upper - 1
        while binom(cand, j) > remaining:
            cand -= 1
        compact.append(cand)
        remaining -= binom(cand, j)
        upper = cand
        j -= 1
    compact.reverse()
    mask = 0
    for c in compact:
        mask |= 1 << avail[c]
    return mask


def mask_rank_batch(subs: np.ndarray, universe: int) -> np.ndarray:
    """Vectorized mask_rank for a fixed universe over an array of subsets."""
    subs = np.asarray(subs, dtype=np.int64)
    r = np.zeros(subs.shape, dtype=np.int64)
    j = np.zeros(subs.shape, dtype=np.int64)
    compact_idx = 0
    u = universe
    while u:
        p = (u & -u).bit_length() - 1
        u &= u - 1
        chosen = (subs >> p) & 1
        j = j + chosen
        # Only add when chosen; use j (post-increment) as the 1-based index.
        r = r + np.where(chosen == 1, BINOM[compact_idx, np.clip(j, 0, MAX_N - 1)], 0)
        compact_idx += 1
    return r


def mask_unrank_batch(ranks: np.ndarray, k: int, universe: int) -> np.ndarray:
    """Vectorized mask_unrank for a fixed universe and fixed subset size k."""
    avail = []
    u = universe
    while u:
        p = (u & -u).bit_length() - 1
        u &= u - 1
        avail.append(p)
    avail = np.array(avail, dtype=np.int64)
    m = len(avail)

    ranks = np.asarray(ranks, dtype=np.int64).copy()
    n = ranks.shape[0]
    mask = np.zeros(n, dtype=np.uint32)

    for j in range(k, 0, -1):
        # BINOM[c][j] is strictly increasing in c, so the largest cand with
        # BINOM[cand][j] <= remaining is a vectorized binary search -- no
        # per-row Python loop needed (this replaced a linear decrement scan
        # that dominated dataset sampling time; see git history).
        col = BINOM[:m, j]
        cand = np.searchsorted(col, ranks, side="right") - 1
        ranks = ranks - col[cand]
        chosen_points = avail[cand]
        mask |= (np.uint32(1) << chosen_points.astype(np.uint32))
    return mask


# ---------------------------------------------------------------------------
# Position <-> (subspace, index)
# ---------------------------------------------------------------------------


def index(mover: int, opp: int) -> tuple[int, int, int]:
    """(w, b, idx) for a position, canonicalizing first."""
    cm, co, _ = symmetry.canonicalize(mover, opp)
    w = bin(cm).count("1")
    b = bin(co).count("1")
    sets = canonical_white_sets()[w]
    white_rank = int(np.searchsorted(sets, cm))
    assert sets[white_rank] == cm, "canonicalize() must land on a canonical white set"
    universe = FULL_MASK & ~cm
    black_rank = mask_rank(co, universe)
    c_universe = binom(24 - w, b)
    return w, b, white_rank * c_universe + black_rank


def unindex(w: int, b: int, idx: int) -> tuple[int, int]:
    """A position occupying slot `idx` of subspace (w, b). May be non-canonical
    (a wasted slot) -- see `is_canonical_slot`.
    """
    c_universe = binom(24 - w, b)
    white_rank, black_rank = divmod(idx, c_universe)
    white = int(canonical_white_sets()[w][white_rank])
    universe = FULL_MASK & ~white
    black = mask_unrank(black_rank, b, universe)
    return white, black


def is_canonical_slot(w: int, b: int, idx: int) -> bool:
    mover, opp = unindex(w, b, idx)
    return symmetry.is_canonical(mover, opp)


def unindex_batch(w: int, b: int, idx: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    idx = np.asarray(idx, dtype=np.int64)
    c_universe = binom(24 - w, b)
    white_rank = idx // c_universe
    black_rank = idx % c_universe
    sets = canonical_white_sets()[w]
    white = sets[white_rank]
    # universe depends on white, which varies per-row -> unrank per unique white.
    black = np.zeros(idx.shape, dtype=np.uint32)
    uniq_whites, inverse = np.unique(white, return_inverse=True)
    for gi, wmask in enumerate(uniq_whites.tolist()):
        sel = inverse == gi
        uni = FULL_MASK & ~wmask
        black[sel] = mask_unrank_batch(black_rank[sel], b, uni)
    return white.astype(np.uint32), black
