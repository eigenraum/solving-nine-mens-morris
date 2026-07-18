"""Board geometry: point numbering, adjacency, mills.

Independent Python port of `src/board.rs` / `readme-database.md` §1. Deliberately
rewritten from the spec rather than transliterated, so that agreement with the Rust
tables (checked in tests) is real evidence of correctness, not copied bugs.

24 points on three concentric rings (0=outer, 1=middle, 2=inner), 8 per ring. Point
``p = ring*8 + i``; within a ring, ``i`` runs clockwise from a fixed corner. Even
``i`` are corners (degree 2), odd ``i`` are edge midpoints that also carry the
"spoke" edges connecting rings at the same ``i`` (degree 3, except the middle ring's
odd points which touch both spokes and are degree 4).
"""

from __future__ import annotations

import numpy as np

N = 24
N_MILLS = 16
FULL_MASK = (1 << N) - 1


def _point(ring: int, i: int) -> int:
    return ring * 8 + (i % 8)


def _build_adj() -> np.ndarray:
    adj = np.zeros(N, dtype=np.uint32)
    for ring in range(3):
        for i in range(8):
            p = _point(ring, i)
            adj[p] |= np.uint32(1) << np.uint32(_point(ring, (i + 1) % 8))
            adj[p] |= np.uint32(1) << np.uint32(_point(ring, (i + 7) % 8))
    for i in range(1, 8, 2):
        a, b, c = _point(0, i), _point(1, i), _point(2, i)
        adj[a] |= np.uint32(1) << np.uint32(b)
        adj[b] |= np.uint32(1) << np.uint32(a)
        adj[b] |= np.uint32(1) << np.uint32(c)
        adj[c] |= np.uint32(1) << np.uint32(b)
    return adj


def _build_mills() -> np.ndarray:
    mills = []
    for ring in range(3):
        for i in range(0, 8, 2):
            a, b, c = _point(ring, i), _point(ring, i + 1), _point(ring, i + 2)
            mills.append((1 << a) | (1 << b) | (1 << c))
    for i in range(1, 8, 2):
        a, b, c = _point(0, i), _point(1, i), _point(2, i)
        mills.append((1 << a) | (1 << b) | (1 << c))
    assert len(mills) == N_MILLS
    return np.array(mills, dtype=np.uint32)


ADJ = _build_adj()
MILLS = _build_mills()


def _build_point_mills() -> list[list[int]]:
    pm: list[list[int]] = [[] for _ in range(N)]
    for m in MILLS.tolist():
        for p in range(N):
            if m & (1 << p):
                pm[p].append(m)
    for p in range(N):
        assert len(pm[p]) == 2, f"point {p} in {len(pm[p])} mills, expected 2"
    return pm


POINT_MILLS = _build_point_mills()


def popcount(mask: np.ndarray) -> np.ndarray:
    """Vectorized popcount (SWAR) over an array of uint32 masks.

    Fast enough to run over all 2**24 24-bit masks in a fraction of a second —
    needed by `ranking.py` to enumerate canonical white sets.
    """
    x = np.asarray(mask, dtype=np.uint32)
    x = x - ((x >> np.uint32(1)) & np.uint32(0x55555555))
    x = (x & np.uint32(0x33333333)) + ((x >> np.uint32(2)) & np.uint32(0x33333333))
    x = (x + (x >> np.uint32(4))) & np.uint32(0x0F0F0F0F)
    x = (x * np.uint32(0x01010101)) >> np.uint32(24)
    return x.astype(np.int64)
