#!/usr/bin/env python3
"""Print count, median, and population standard deviation for Ferry benchmark CSV."""

import csv
import statistics
import sys
from collections import defaultdict


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else "bench-results.csv"
    groups = defaultdict(list)
    with open(path, newline="", encoding="utf-8") as stream:
        for row in csv.DictReader(stream):
            key = (row["cache"], row["scenario"], row["tool"], row["filesystem"])
            groups[key].append(float(row["seconds"]))
    print("cache,scenario,tool,filesystem,runs,median_seconds,pstdev_seconds")
    for key, values in sorted(groups.items()):
        print(
            ",".join(key),
            len(values),
            f"{statistics.median(values):.6f}",
            f"{statistics.pstdev(values):.6f}",
            sep=",",
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
