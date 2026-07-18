import json
from pathlib import Path

import pytest

from nmm.export import export_geometry, export_rules_trace

DB_DIR = Path(__file__).resolve().parent.parent.parent / "db"
HAVE_DB = (DB_DIR / "manifest.json").exists()


def test_export_geometry_writes_expected_shape(tmp_path):
    export_geometry(str(tmp_path))
    data = json.loads((tmp_path / "geometry.json").read_text())
    assert len(data["adj"]) == 24
    assert len(data["mills"]) == 16
    assert len(data["point_mills"]) == 24
    assert len(data["perms"]) == 16
    assert all(len(row) == 24 for row in data["perms"])


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_export_rules_trace_writes_valid_traces(tmp_path):
    export_rules_trace(str(DB_DIR), str(tmp_path), n_traces=200, seed=0)
    data = json.loads((tmp_path / "rules_trace.json").read_text())
    assert len(data) >= 100  # some slack: sampling can slightly under/overshoot
    for trace in data[:20]:
        assert "mover" in trace and "opp" in trace and "moves" in trace
        for mv in trace["moves"]:
            assert set(mv.keys()) == {
                "src",
                "dst",
                "captured",
                "successorMover",
                "successorOpp",
            }
