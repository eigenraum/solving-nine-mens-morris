"""The lightweight value network: a residual MLP with WDL + depth heads.

design-nn.md §4. No normalization layers (batch-independent inference, trivial
to port to the browser runtime in `web/src/nn.ts`).
"""

from __future__ import annotations

import torch
import torch.nn as nn
import torch.nn.functional as F

from .features import FEATURE_DIM

# (hidden_dim, n_blocks) -- design-nn.md §4's three reference configurations.
CONFIGS = {
    "S": (128, 2),
    "M": (256, 4),
    "L": (384, 6),
}


class ResidualBlock(nn.Module):
    def __init__(self, h: int):
        super().__init__()
        self.fc1 = nn.Linear(h, h)
        self.fc2 = nn.Linear(h, h)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return F.relu(x + self.fc2(F.relu(self.fc1(x))))


class NmmNet(nn.Module):
    """input(52) -> Linear+ReLU -> [ResidualBlock]*n_blocks -> {wdl(3), depth(1)}."""

    def __init__(self, hidden: int = 256, n_blocks: int = 4):
        super().__init__()
        self.hidden = hidden
        self.n_blocks = n_blocks
        self.input = nn.Linear(FEATURE_DIM, hidden)
        self.blocks = nn.ModuleList([ResidualBlock(hidden) for _ in range(n_blocks)])
        self.wdl_head = nn.Linear(hidden, 3)
        self.depth_head = nn.Linear(hidden, 1)

    def forward(self, x: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        z = F.relu(self.input(x))
        for block in self.blocks:
            z = block(z)
        wdl_logits = self.wdl_head(z)
        depth = torch.sigmoid(self.depth_head(z)).squeeze(-1)
        return wdl_logits, depth

    @classmethod
    def from_config(cls, name: str) -> "NmmNet":
        hidden, n_blocks = CONFIGS[name]
        return cls(hidden=hidden, n_blocks=n_blocks)

    def num_params(self) -> int:
        return sum(p.numel() for p in self.parameters())


def loss_fn(
    wdl_logits: torch.Tensor,
    depth_pred: torch.Tensor,
    labels: torch.Tensor,
    depth_target: torch.Tensor,
    depth_mask: torch.Tensor,
    depth_weight: float = 0.25,
) -> tuple[torch.Tensor, dict]:
    """CE(WDL) + depth_weight * masked-MSE(depth) -- design-nn.md §7.

    depth_mask selects decided (non-draw) states only; if none are present in
    a batch, the depth term is skipped for that batch (returns 0 contribution)
    rather than producing a NaN from an empty-mean.
    """
    ce = F.cross_entropy(wdl_logits, labels)
    if depth_mask.any():
        mse = F.mse_loss(depth_pred[depth_mask], depth_target[depth_mask])
    else:
        mse = torch.zeros((), device=wdl_logits.device)
    total = ce + depth_weight * mse
    with torch.no_grad():
        pred_labels = wdl_logits.argmax(dim=-1)
        acc = (pred_labels == labels).float().mean()
    return total, {"ce": ce.item(), "mse": mse.item(), "acc": acc.item()}
