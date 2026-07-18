import numpy as np
import pytest

from nmm.board import ADJ, MILLS, N
from nmm.symmetry import (
    N_SYMS,
    PERMS,
    apply,
    apply_batch,
    canonicalize,
    canonicalize_batch,
    canonicalize_set,
    is_canonical,
    is_canonical_batch,
    stabilizer_size,
)


def test_16_distinct_permutations():
    seen = {tuple(row) for row in PERMS.tolist()}
    assert len(seen) == 16


def test_identity_present():
    ident = list(range(N))
    assert any(list(PERMS[k]) == ident for k in range(N_SYMS))


def test_group_closure():
    # Composing any two of the 16 permutations must land back in the set.
    perm_set = {tuple(row) for row in PERMS.tolist()}
    for i in range(N_SYMS):
        for j in range(N_SYMS):
            composed = tuple(int(PERMS[i][PERMS[j][p]]) for p in range(N))
            assert composed in perm_set


def test_automorphism_preserves_adjacency():
    full_adj = {(p, q) for p in range(N) for q in range(N) if int(ADJ[p]) & (1 << q)}
    for k in range(N_SYMS):
        mapped = {(int(PERMS[k][p]), int(PERMS[k][q])) for (p, q) in full_adj}
        assert mapped == full_adj


def test_automorphism_preserves_mills():
    mill_set = {int(m) for m in MILLS.tolist()}
    for k in range(N_SYMS):
        mapped = {apply(k, m) for m in mill_set}
        assert mapped == mill_set


@pytest.mark.parametrize("trial", range(200))
def test_canonical_invariance_scalar(trial):
    rng = np.random.default_rng(trial)
    all_points = rng.permutation(N)
    w = int(rng.integers(3, 10))
    b = int(rng.integers(3, 10 - 0))
    b = min(b, N - w)
    mover = 0
    for p in all_points[:w]:
        mover |= 1 << int(p)
    opp = 0
    for p in all_points[w : w + b]:
        opp |= 1 << int(p)
    base = canonicalize(mover, opp)[:2]
    k = int(rng.integers(0, N_SYMS))
    m2, o2 = apply(k, mover), apply(k, opp)
    assert canonicalize(m2, o2)[:2] == base


def test_canonical_invariance_batch():
    rng = np.random.default_rng(42)
    n = 2000
    movers = np.zeros(n, dtype=np.uint32)
    opps = np.zeros(n, dtype=np.uint32)
    for i in range(n):
        pts = rng.permutation(N)
        w = int(rng.integers(3, 10))
        b = int(min(rng.integers(3, 10), N - w))
        m = 0
        for p in pts[:w]:
            m |= 1 << int(p)
        o = 0
        for p in pts[w : w + b]:
            o |= 1 << int(p)
        movers[i] = m
        opps[i] = o

    base_m, base_o, _ = canonicalize_batch(movers, opps)

    ks = rng.integers(0, N_SYMS, size=n)
    m2 = np.zeros(n, dtype=np.uint32)
    o2 = np.zeros(n, dtype=np.uint32)
    for k in range(N_SYMS):
        sel = ks == k
        if not sel.any():
            continue
        am, ao = apply_batch(k, movers[sel]), apply_batch(k, opps[sel])
        m2[sel] = am
        o2[sel] = ao

    can_m2, can_o2, _ = canonicalize_batch(m2, o2)
    assert (can_m2 == base_m).all()
    assert (can_o2 == base_o).all()


def test_scalar_and_batch_agree():
    rng = np.random.default_rng(7)
    n = 500
    movers = rng.integers(0, 1 << 24, size=n, dtype=np.int64).astype(np.uint32)
    opps = rng.integers(0, 1 << 24, size=n, dtype=np.int64).astype(np.uint32)
    # ensure disjoint
    opps = opps & ~movers

    can_m_batch, can_o_batch, _ = canonicalize_batch(movers, opps)
    for i in range(n):
        cm, co, _ = canonicalize(int(movers[i]), int(opps[i]))
        assert cm == int(can_m_batch[i])
        assert co == int(can_o_batch[i])
        assert is_canonical(cm, co)

    ic_batch = is_canonical_batch(can_m_batch, can_o_batch)
    assert ic_batch.all()


def test_canonicalize_set_matches_min_over_orbit():
    rng = np.random.default_rng(3)
    for _ in range(500):
        w = int(rng.integers(3, 10))
        pts = rng.permutation(N)[:w]
        mask = 0
        for p in pts:
            mask |= 1 << int(p)
        expected = min(apply(k, mask) for k in range(N_SYMS))
        assert canonicalize_set(mask) == expected


def test_stabilizer_size_divides_16():
    rng = np.random.default_rng(11)
    for _ in range(200):
        w = int(rng.integers(3, 10))
        b = int(min(rng.integers(3, 10), N - w))
        pts = rng.permutation(N)
        mover = 0
        for p in pts[:w]:
            mover |= 1 << int(p)
        opp = 0
        for p in pts[w : w + b]:
            opp |= 1 << int(p)
        s = stabilizer_size(mover, opp)
        assert 1 <= s <= 16
        assert 16 % s == 0
