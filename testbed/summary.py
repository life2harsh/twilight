import csv
import statistics
import sys
from pathlib import Path


def summarize(path):
    rows = list(csv.DictReader(path.open()))
    stats = [r for r in rows if r["event"] == "stats"]
    if not stats:
        return f"{path.stem}: no stats"
    tail = stats[len(stats) // 4:]
    fps = statistics.mean(int(r["rx_fps_x10"]) / 10 for r in tail)
    completion = statistics.mean(int(r["completion_pm"]) / 10 for r in tail)
    quality = statistics.mean(int(r["quality"]) for r in tail)
    seconds = (int(stats[-1]["t_us"]) - int(stats[0]["t_us"])) / 1e6 or 1
    mbps = (int(stats[-1]["bytes"]) - int(stats[0]["bytes"])) * 8 / seconds / 1e6
    moves = [f"{r['event'][8:]}->{r['transport']}@{(int(r['t_us']) - int(rows[0]['t_us'])) / 1e6:.1f}s" for r in rows if r["event"].startswith("migrate")]
    return f"{path.stem:18} rx {fps:5.1f} fps  done {completion:5.1f}%  q {quality:4.1f}  {mbps:6.1f} Mbps  {' '.join(moves)}"


for name in sys.argv[1:]:
    print(summarize(Path(name)))
