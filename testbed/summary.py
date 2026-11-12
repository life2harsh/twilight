import csv
import statistics
import sys
from pathlib import Path


def metrics(path):
    rows = list(csv.DictReader(path.open()))
    stats = [r for r in rows if r["event"] == "stats"]
    if not stats:
        return None
    tail = stats[len(stats) // 4:]
    seconds = (int(stats[-1]["t_us"]) - int(stats[0]["t_us"])) / 1e6 or 1
    return {
        "fps": statistics.mean(int(r["rx_fps_x10"]) / 10 for r in tail),
        "completion": statistics.mean(int(r["completion_pm"]) / 10 for r in tail),
        "quality": statistics.mean(int(r["quality"]) for r in tail),
        "mbps": (int(stats[-1]["bytes"]) - int(stats[0]["bytes"])) * 8 / seconds / 1e6,
        "moves": [f"{r['event'][8:]}->{r['transport']}@{(int(r['t_us']) - int(rows[0]['t_us'])) / 1e6:.1f}s" for r in rows if r["event"].startswith("migrate")],
    }


def summarize(path):
    m = metrics(path)
    if m is None:
        return f"{path.stem}: no stats"
    return f"{path.stem:18} rx {m['fps']:5.1f} fps  done {m['completion']:5.1f}%  q {m['quality']:4.1f}  {m['mbps']:6.1f} Mbps  {' '.join(m['moves'])}"


if __name__ == "__main__":
    for name in (n for n in sys.argv[1:] if not n.endswith(".client.csv")):
        print(summarize(Path(name)))
