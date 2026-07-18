import json
import struct
from pathlib import Path

import pytest
import torch

from nmm.export import export_model
from nmm.features import FEATURE_DIM
from nmm.model import NmmNet


@pytest.fixture
def dummy_checkpoint(tmp_path):
    model = NmmNet.from_config("S")
    path = tmp_path / "dummy.pt"
    torch.save(
        {
            "config": "S",
            "hidden": model.hidden,
            "n_blocks": model.n_blocks,
            "state_dict": model.state_dict(),
            "n_samples": 0,
        },
        path,
    )
    return str(path), model


def test_export_model_writes_all_artifacts(tmp_path, dummy_checkpoint):
    ckpt_path, _ = dummy_checkpoint
    out_dir = tmp_path / "export"
    export_model(ckpt_path, str(out_dir), n_golden=8)

    assert (out_dir / "model.onnx").exists()
    assert (out_dir / "model.bin").exists()
    assert (out_dir / "model.json").exists()
    assert (out_dir / "golden.json").exists()


def test_export_model_blob_size_matches_manifest(tmp_path, dummy_checkpoint):
    ckpt_path, model = dummy_checkpoint
    out_dir = tmp_path / "export"
    export_model(ckpt_path, str(out_dir), n_golden=4)

    manifest = json.loads((out_dir / "model.json").read_text())
    blob = (out_dir / "model.bin").read_bytes()

    expected_bytes = 0
    for layer in manifest["layers"]:
        expected_bytes += (layer["out"] * layer["in"] + layer["out"]) * 4  # float32
    assert len(blob) == expected_bytes
    assert manifest["feature_dim"] == FEATURE_DIM
    assert manifest["hidden"] == model.hidden
    assert manifest["n_blocks"] == model.n_blocks


def test_golden_vectors_match_live_forward_pass(tmp_path, dummy_checkpoint):
    ckpt_path, model = dummy_checkpoint
    out_dir = tmp_path / "export"
    export_model(ckpt_path, str(out_dir), n_golden=16, seed=42)

    golden = json.loads((out_dir / "golden.json").read_text())
    x = torch.tensor(golden["inputs"], dtype=torch.float32)
    model.eval()
    with torch.no_grad():
        logits, depth = model(x)
    assert torch.allclose(logits, torch.tensor(golden["wdl_logits"]), atol=1e-6)
    assert torch.allclose(depth, torch.tensor(golden["depth"]), atol=1e-6)


def test_blob_layer_order_matches_manual_unpack(tmp_path, dummy_checkpoint):
    """Manually parse model.bin per model.json's layer order and confirm the
    first layer's weight matrix matches the checkpoint's input layer -- this
    is exactly what web/src/nn.ts's loader does, just in Python."""
    ckpt_path, model = dummy_checkpoint
    out_dir = tmp_path / "export"
    export_model(ckpt_path, str(out_dir), n_golden=4)

    manifest = json.loads((out_dir / "model.json").read_text())
    blob = (out_dir / "model.bin").read_bytes()

    offset = 0
    first = manifest["layers"][0]
    n_w = first["out"] * first["in"]
    w = struct.unpack_from(f"<{n_w}f", blob, offset)
    offset += n_w * 4
    b = struct.unpack_from(f"<{first['out']}f", blob, offset)

    expected_w = model.input.weight.detach().numpy().flatten()
    expected_b = model.input.bias.detach().numpy().flatten()
    assert list(w) == pytest.approx(list(expected_w), abs=1e-6)
    assert list(b) == pytest.approx(list(expected_b), abs=1e-6)
