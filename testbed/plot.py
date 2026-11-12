import csv
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

from summary import metrics

results = Path(sys.argv[1])
conditions, modes = sys.argv[2].split(), sys.argv[3].split()
block_at, unblock_at = float(sys.argv[4]), float(sys.argv[5])
figures = results / "figures"
figures.mkdir(exist_ok=True)

fig, axes = plt.subplots(1, 2, figsize=(12, 4.5))
width = 0.8 / len(modes)
for j, mode in enumerate(modes):
    found = [metrics(results / f"{c}_{mode}.csv") if (results / f"{c}_{mode}.csv").exists() else None for c in conditions]
    for ax, key in zip(axes, ("fps", "completion")):
        ax.bar([i + j * width for i in range(len(conditions))], [m[key] if m else 0 for m in found], width, label=mode)
for ax, title in zip(axes, ("Received fps", "Frames completed (%)")):
    ax.set_xticks([i + width * (len(modes) - 1) / 2 for i in range(len(conditions))], conditions)
    ax.set_title(title)
    ax.grid(axis="y", alpha=0.3)
axes[0].legend()
fig.tight_layout()
fig.savefig(figures / "modes.png", dpi=150)

rows = list(csv.DictReader((results / "blocked.client.csv").open()))
t0 = float(rows[0]["time_s"])
t = [float(r["time_s"]) - t0 for r in rows]
blocked = float((results / "blocked.blocked_at").read_text()) - t0
fig, ax = plt.subplots(figsize=(10, 4))
ax.plot(t, [float(r["fps"]) for r in rows], color="black", label="fps")
for i in range(len(rows) - 1):
    ax.axvspan(t[i], t[i + 1], color="tab:blue" if rows[i]["transport"] == "UDP" else "tab:orange", alpha=0.15, lw=0)
ax.axvline(blocked, color="red", ls="--", label="UDP blocked")
ax.axvline(blocked + unblock_at - block_at, color="green", ls="--", label="UDP unblocked")
ax.set(xlabel="time (s)", ylabel="fps", title="Migration: blue = UDP, orange = TCP")
ax.legend()
fig.tight_layout()
fig.savefig(figures / "migration.png", dpi=150)

traces = sorted(results.glob("ns2_*.dat"), key=lambda p: float(p.stem[4:]))
if traces:
    fig, axes = plt.subplots(len(traces), 1, figsize=(10, 3 * len(traces)), sharex=True, squeeze=False)
    for ax, path in zip(axes[:, 0], traces):
        cols = list(zip(*(map(float, line.split()) for line in path.read_text().split("\n") if line)))
        ax.plot(cols[0], cols[1], label="TCP Reno")
        ax.plot(cols[0], cols[2], label="UDP CBR 6 Mb/s")
        ax.set(ylabel="Mb/s", title=f"ns-2 bottleneck, loss {path.stem[4:]}")
        ax.grid(alpha=0.3)
        ax.legend()
    axes[-1, 0].set_xlabel("time (s)")
    fig.tight_layout()
    fig.savefig(figures / "ns2.png", dpi=150)

print("\n".join(str(p) for p in sorted(figures.glob("*.png"))))
