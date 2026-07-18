"""Training CLI. design-nn.md §7 / implementation-nn.md N5.

Usage:
    python -m nmm.train --db ../db --config M --samples 200000000 --out run.pt

The model is tiny; throughput is loader-bound (see dataset.py), so a modest
number of DataLoader workers and a large batch size are both cheap.
"""

from __future__ import annotations

import argparse
import math
import time
from pathlib import Path

import torch
from torch.utils.data import DataLoader

from .dataset import NmmIterableDataset
from .model import NmmNet, loss_fn


def build_loader(
    db_dir: str,
    split: str,
    batch_size: int,
    seed: int,
    num_workers: int,
    only_pairs: list[tuple[int, int]] | None = None,
) -> DataLoader:
    ds = NmmIterableDataset(
        db_dir, split=split, batch_size=batch_size, seed=seed, only_pairs=only_pairs
    )
    return DataLoader(ds, batch_size=None, num_workers=num_workers)


@torch.no_grad()
def evaluate_val(model: NmmNet, loader, device: str, n_batches: int) -> dict:
    model.eval()
    it = iter(loader)
    total_acc = 0.0
    total_ce = 0.0
    total_mse = 0.0
    n = 0
    for _ in range(n_batches):
        feats, labels, depth_t, depth_mask = next(it)
        feats, labels = feats.to(device), labels.to(device)
        depth_t, depth_mask = depth_t.to(device), depth_mask.to(device)
        wdl_logits, depth_pred = model(feats)
        _, stats = loss_fn(wdl_logits, depth_pred, labels, depth_t, depth_mask)
        total_acc += stats["acc"]
        total_ce += stats["ce"]
        total_mse += stats["mse"]
        n += 1
    model.train()
    return {"val_acc": total_acc / n, "val_ce": total_ce / n, "val_mse": total_mse / n}


def train(
    db_dir: str,
    config: str,
    n_samples: int,
    batch_size: int = 8192,
    lr: float = 1e-3,
    weight_decay: float = 1e-2,
    depth_weight: float = 0.25,
    num_workers: int = 2,
    seed: int = 0,
    device: str = "cpu",
    out_path: str | None = None,
    log_every: int = 20,
    val_every: int = 200,
    only_pairs: list[tuple[int, int]] | None = None,
    logdir: str | None = None,
) -> NmmNet:
    writer = None
    if logdir:
        from torch.utils.tensorboard.writer import SummaryWriter

        writer = SummaryWriter(logdir)

    model = NmmNet.from_config(config).to(device)
    n_steps = max(1, math.ceil(n_samples / batch_size))

    opt = torch.optim.AdamW(model.parameters(), lr=lr, weight_decay=weight_decay)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=n_steps, eta_min=1e-5)

    train_loader = build_loader(db_dir, "train", batch_size, seed, num_workers, only_pairs)
    val_loader = build_loader(
        db_dir, "val", batch_size, seed + 1, max(1, num_workers // 2), only_pairs
    )

    train_it = iter(train_loader)
    start = time.time()
    step = 0
    n_seen = 0
    while n_seen < n_samples:
        feats, labels, depth_t, depth_mask = next(train_it)
        feats, labels = feats.to(device), labels.to(device)
        depth_t, depth_mask = depth_t.to(device), depth_mask.to(device)

        wdl_logits, depth_pred = model(feats)
        loss, stats = loss_fn(wdl_logits, depth_pred, labels, depth_t, depth_mask, depth_weight)

        opt.zero_grad()
        loss.backward()
        torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
        opt.step()
        sched.step()

        step += 1
        n_seen += feats.shape[0]

        if step % log_every == 0:
            elapsed = time.time() - start
            rate = n_seen / elapsed
            print(
                f"step {step}/{n_steps} seen={n_seen} loss={loss.item():.4f} "
                f"acc={stats['acc']:.4f} ce={stats['ce']:.4f} mse={stats['mse']:.4f} "
                f"lr={sched.get_last_lr()[0]:.2e} rate={rate:.0f}/s"
            )
            if writer:
                writer.add_scalar("train/loss", loss.item(), n_seen)
                writer.add_scalar("train/acc", stats["acc"], n_seen)
                writer.add_scalar("train/ce", stats["ce"], n_seen)
                writer.add_scalar("train/mse", stats["mse"], n_seen)
                writer.add_scalar("train/lr", sched.get_last_lr()[0], n_seen)
                writer.add_scalar("train/samples_per_sec", rate, n_seen)
        if step % val_every == 0:
            val_stats = evaluate_val(model, val_loader, device, n_batches=5)
            print(f"  [val] {val_stats}")
            if writer:
                writer.add_scalar("val/acc", val_stats["val_acc"], n_seen)
                writer.add_scalar("val/ce", val_stats["val_ce"], n_seen)
                writer.add_scalar("val/mse", val_stats["val_mse"], n_seen)

    if out_path:
        torch.save(
            {
                "config": config,
                "hidden": model.hidden,
                "n_blocks": model.n_blocks,
                "state_dict": model.state_dict(),
                "n_samples": n_seen,
            },
            out_path,
        )
        print(f"saved checkpoint to {out_path}")

    if writer:
        writer.close()

    return model


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--db", required=True, help="path to db/ directory")
    p.add_argument("--config", choices=["S", "M", "L"], default="M")
    p.add_argument("--samples", type=int, default=200_000_000)
    p.add_argument("--batch-size", type=int, default=8192)
    p.add_argument("--lr", type=float, default=1e-3)
    p.add_argument("--weight-decay", type=float, default=1e-2)
    p.add_argument("--depth-weight", type=float, default=0.25)
    p.add_argument("--num-workers", type=int, default=2)
    p.add_argument("--seed", type=int, default=0)
    p.add_argument("--device", default="cpu")
    p.add_argument("--out", default=None)
    p.add_argument("--log-every", type=int, default=20)
    p.add_argument("--val-every", type=int, default=200)
    p.add_argument(
        "--logdir",
        default=None,
        help="TensorBoard log directory, e.g. runs/M_3e8 (needs the tensorboard "
        "package; logs the same stats as the console, keyed by samples seen)",
    )
    p.add_argument(
        "--only-pairs",
        default=None,
        help='restrict to specific subspaces, e.g. "3,3" or "3,3;4,4" '
        "(memorization sanity check: --only-pairs 3,3 --config S)",
    )
    args = p.parse_args()

    only_pairs = None
    if args.only_pairs:
        only_pairs = [
            tuple(int(x) for x in pair.split(",")) for pair in args.only_pairs.split(";")
        ]

    train(
        db_dir=args.db,
        config=args.config,
        n_samples=args.samples,
        batch_size=args.batch_size,
        lr=args.lr,
        weight_decay=args.weight_decay,
        depth_weight=args.depth_weight,
        num_workers=args.num_workers,
        seed=args.seed,
        device=args.device,
        out_path=args.out,
        log_every=args.log_every,
        val_every=args.val_every,
        only_pairs=only_pairs,
        logdir=args.logdir,
    )


if __name__ == "__main__":
    main()
