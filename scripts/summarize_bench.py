#!/usr/bin/env python3
"""Summarize a ripsync benchmark CSV.

Prints, per (cache, scenario, tool, filesystem) group: run count, median, mean,
population standard deviation, minimum, and the 95th percentile of wall time.
"""

import csv
import statistics
import sys
from collections import defaultdict


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = pct / 100 * (len(ordered) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (rank - lo)


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "bench-results.csv"
    groups: dict[tuple[str, ...], list[float]] = defaultdict(list)
    with open(path, newline="", encoding="utf-8") as stream:
        for row in csv.DictReader(stream):
            key = (row["cache"], row["scenario"], row["tool"], row["filesystem"])
            groups[key].append(float(row["seconds"]))
    print("cache,scenario,tool,filesystem,runs,median_s,mean_s,pstdev_s,min_s,p95_s")
    for key, values in sorted(groups.items()):
        print(
            ",".join(key),
            len(values),
            f"{statistics.median(values):.6f}",
            f"{statistics.fmean(values):.6f}",
            f"{statistics.pstdev(values):.6f}",
            f"{min(values):.6f}",
            f"{percentile(values, 95):.6f}",
            sep=",",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
