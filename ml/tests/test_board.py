from nmm.board import ADJ, MILLS, N, N_MILLS, POINT_MILLS, popcount


def test_degrees():
    degrees = [bin(int(ADJ[p])).count("1") for p in range(N)]
    assert degrees.count(2) == 12
    assert degrees.count(3) == 8
    assert degrees.count(4) == 4
    assert sum(degrees) == 2 * 32  # 32 edges total, each contributes 2 endpoints


def test_mill_count_and_size():
    assert len(MILLS) == N_MILLS
    for m in MILLS.tolist():
        assert bin(m).count("1") == 3


def test_every_point_in_exactly_two_mills():
    for p in range(N):
        assert len(POINT_MILLS[p]) == 2
        for m in POINT_MILLS[p]:
            assert m & (1 << p)


def test_adjacency_symmetric():
    for p in range(N):
        for q in range(N):
            if int(ADJ[p]) & (1 << q):
                assert int(ADJ[q]) & (1 << p), f"{p}->{q} not reciprocated"


def test_popcount_matches_python_builtin():
    import numpy as np

    rng = np.random.default_rng(0)
    masks = rng.integers(0, 1 << 24, size=10_000, dtype=np.int64).astype(np.uint32)
    expected = np.array([bin(int(m)).count("1") for m in masks])
    assert (popcount(masks) == expected).all()
