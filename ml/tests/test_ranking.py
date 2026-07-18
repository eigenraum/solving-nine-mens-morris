from pathlib import Path

import numpy as np
import pytest

from nmm import ranking, symmetry
from nmm.board import FULL_MASK

DB_DIR = Path(__file__).resolve().parent.parent.parent / "db"
HAVE_DB = (DB_DIR / "manifest.json").exists()


def test_canonical_set_sizes_match_spec():
    sets = ranking.canonical_white_sets()
    for w, expected in ranking.EXPECTED_CANONICAL_SIZES.items():
        assert len(sets[w]) == expected, f"w={w}"


def test_canonical_sets_sorted_and_actually_canonical():
    sets = ranking.canonical_white_sets()
    for w, arr in sets.items():
        assert (np.diff(arr.astype(np.int64)) > 0).all(), f"w={w} not sorted"
        for mask in arr[:: max(1, len(arr) // 50)].tolist():
            assert symmetry.canonicalize_set(int(mask)) == mask

    # every canonical set has the claimed popcount
    from nmm.board import popcount

    for w, arr in sets.items():
        assert (popcount(arr) == w).all()


def test_size_3_3_matches_manifest():
    assert ranking.subspace_size(3, 3) == 210140


def test_binom_matches_math_comb():
    import math

    for n in range(25):
        for k in range(n + 1):
            assert ranking.binom(n, k) == math.comb(n, k)


@pytest.mark.parametrize("trial", range(300))
def test_mask_rank_unrank_bijection_small_universe(trial):
    rng = np.random.default_rng(trial)
    universe_size = int(rng.integers(4, 13))
    universe = (1 << universe_size) - 1  # first `universe_size` points
    k = int(rng.integers(0, universe_size + 1))
    # exhaustive check for this (universe_size, k)
    from itertools import combinations

    all_subsets = list(combinations(range(universe_size), k))
    ranks = set()
    for combo in all_subsets:
        mask = 0
        for p in combo:
            mask |= 1 << p
        r = ranking.mask_rank(mask, universe)
        assert r not in ranks, "rank collision"
        ranks.add(r)
        assert ranking.mask_unrank(r, k, universe) == mask
    assert ranks == set(range(len(all_subsets)))


def test_mask_rank_unrank_batch_matches_scalar():
    rng = np.random.default_rng(5)
    universe = FULL_MASK & ~0b111  # 21-point universe (drop first 3 points)
    k = 5
    from itertools import combinations

    pts = [p for p in range(24) if universe & (1 << p)]
    combos = list(combinations(pts, k))
    sample = rng.choice(len(combos), size=500, replace=False)
    subs = []
    for i in sample:
        mask = 0
        for p in combos[i]:
            mask |= 1 << p
        subs.append(mask)
    subs = np.array(subs, dtype=np.int64)

    ranks_scalar = np.array([ranking.mask_rank(int(s), universe) for s in subs])
    ranks_batch = ranking.mask_rank_batch(subs, universe)
    assert (ranks_scalar == ranks_batch).all()

    unranked_batch = ranking.mask_unrank_batch(ranks_batch, k, universe)
    assert (unranked_batch.astype(np.int64) == subs).all()


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_index_roundtrip_on_real_subspaces():
    from nmm.db import Database

    d = Database(DB_DIR)
    rng = np.random.default_rng(1)
    for w, b in d.available_pairs:
        n = d.size(w, b)
        idx = rng.integers(0, n, size=min(2000, n))
        mover, opp = ranking.unindex_batch(w, b, idx)
        canon = symmetry.is_canonical_batch(mover, opp)
        checked = 0
        for i in range(len(idx)):
            if not canon[i]:
                continue
            w2, b2, idx2 = ranking.index(int(mover[i]), int(opp[i]))
            assert (w2, b2) == (w, b)
            assert idx2 == int(idx[i])
            checked += 1
        assert checked > 0, f"no canonical slots sampled for ({w},{b})"


@pytest.mark.slow
@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_tally_matches_ninemm_db_stats():
    """The strongest N2 gate: full-scan win/loss/draw tallies (over canonical
    slots) must exactly match `ninemm db-stats`'s output, captured in
    ml/cache/db-stats-partial.txt by N0. Any mismatch means the symmetry,
    ranking, or decoding logic disagrees with the Rust implementation.
    Slow (~6 min): scans all ~166M states in the partial dev database.
    """
    from nmm.db import Database

    baseline_path = Path(__file__).resolve().parent.parent / "cache" / "db-stats-partial.txt"
    if not baseline_path.exists():
        pytest.skip("db-stats-partial.txt baseline not captured")
    baseline = {}
    for line in baseline_path.read_text().splitlines():
        if not line.strip():
            continue
        head, rest = line.split(":")
        w, b = (int(x) for x in head.split("-"))
        fields = dict(kv.split("=") for kv in rest.split())
        baseline[(w, b)] = {k: int(v) for k, v in fields.items()}

    d = Database(DB_DIR)
    for w, b in d.available_pairs:
        got = d.tally(w, b)
        exp = baseline[(w, b)]
        assert got == exp, f"mismatch on ({w},{b}): got {got} expected {exp}"
