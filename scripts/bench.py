#!/usr/bin/env python3
"""Cross-platform ripsync benchmark harness (macOS + Linux).

Generates the requested datasets, records raw CSV metrics, and verifies content
plus mode/mtime/symlink metadata after every timed run. ripsync is measured both
with copy-on-write reflinks enabled (`--reflink auto`) and disabled
(`--reflink never`) so the engine-vs-engine result is reported separately from
the "we have reflink, rsync does not" result. The comparison keeps durability
equal: `rsync -a` and ripsync's default both skip per-file fsync.

Configuration is via environment variables (all optional):

  RIPSYNC            path to the ripsync binary (default target/release/ripsync)
  RSYNC              path to rsync           (default: Homebrew rsync, else PATH)
  RUNS               repetitions per config  (default 10)
  CACHE_MODE         warm | cold | both      (default warm)
  BENCH_SCENARIOS    comma list of: tiny-small,tiny-large,large,resync
                                             (default all)
  BENCH_ROOT         scratch directory       (default <repo>/.bench)
  BENCH_RESULTS      output CSV              (default <repo>/bench-results.csv)
  BENCH_APPEND       1 to append to an existing CSV (default: rewrite)

  Dataset sizing (override to scale up/down):
  TINY_SMALL_COUNT   default 100000
  TINY_LARGE_COUNT   default 500000
  LARGE_FILES        default 250
  LARGE_FILE_MIB     default 20      (=> LARGE_FILES * LARGE_FILE_MIB total)
  RESYNC_COUNT       default 500000
  RESYNC_CHANGED     default 100

Cold-cache runs require privileges to drop the page cache (`purge` on macOS,
/proc/sys/vm/drop_caches on Linux); a run is never labelled "cold" unless the
drop actually succeeded.
"""

from __future__ import annotations

import os
import pathlib
import platform
import shutil
import stat
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
IS_DARWIN = platform.system() == "Darwin"


def env(name: str, default: str) -> str:
    return os.environ.get(name, default)


def env_int(name: str, default: int) -> int:
    return int(os.environ.get(name, default))


RIPSYNC = pathlib.Path(env("RIPSYNC", str(ROOT / "target" / "release" / "ripsync")))
RUNS = env_int("RUNS", 10)
CACHE_MODE = env("CACHE_MODE", "warm")
SCENARIOS = env("BENCH_SCENARIOS", "tiny-small,tiny-large,large,resync").split(",")
BENCH_ROOT = pathlib.Path(env("BENCH_ROOT", str(ROOT / ".bench")))
RESULTS = pathlib.Path(env("BENCH_RESULTS", str(ROOT / "bench-results.csv")))

TINY_SMALL_COUNT = env_int("TINY_SMALL_COUNT", 100_000)
TINY_LARGE_COUNT = env_int("TINY_LARGE_COUNT", 500_000)
LARGE_FILES = env_int("LARGE_FILES", 250)
LARGE_FILE_MIB = env_int("LARGE_FILE_MIB", 20)
RESYNC_COUNT = env_int("RESYNC_COUNT", 500_000)
RESYNC_CHANGED = env_int("RESYNC_CHANGED", 100)


def find_rsync() -> str:
    explicit = os.environ.get("RSYNC")
    if explicit:
        return explicit
    if IS_DARWIN and pathlib.Path("/opt/homebrew/bin/rsync").exists():
        # Prefer Homebrew's modern rsync over Apple's openrsync 2.6.9 shim.
        return "/opt/homebrew/bin/rsync"
    found = shutil.which("rsync")
    if not found:
        sys.exit("rsync is required")
    return found


RSYNC = find_rsync()


def fs_type(path: pathlib.Path) -> str:
    try:
        if IS_DARWIN:
            return subprocess.check_output(
                ["stat", "-f", "%T", str(path)], text=True
            ).strip()
        return subprocess.check_output(
            ["findmnt", "-n", "-T", str(path), "-o", "FSTYPE"], text=True
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def tool_version(cmd: list[str]) -> str:
    try:
        out = subprocess.check_output(cmd, text=True, stderr=subprocess.STDOUT)
        return out.splitlines()[0].strip()
    except (subprocess.CalledProcessError, FileNotFoundError, IndexError):
        return "unknown"


def git_rev() -> str:
    try:
        return subprocess.check_output(
            ["git", "-C", str(ROOT), "rev-parse", "--short", "HEAD"], text=True
        ).strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "unknown"


def drop_caches() -> bool:
    """Drop the OS page cache. Returns True only if it actually succeeded."""
    if IS_DARWIN:
        return subprocess.run(["purge"], capture_output=True).returncode == 0
    try:
        with open("/proc/sys/vm/drop_caches", "w", encoding="ascii") as handle:
            handle.write("3\n")
        return True
    except OSError:
        result = subprocess.run(
            ["sudo", "-n", "sh", "-c", "echo 3 > /proc/sys/vm/drop_caches"],
            capture_output=True,
        )
        return result.returncode == 0


def gen_tiny(root: pathlib.Path, count: int) -> None:
    marker = root.parent / f"{root.name}.complete-{count}"
    if marker.exists():
        return
    if root.exists():
        shutil.rmtree(root)
    print(f">> generating {count} tiny files in {root}", flush=True)
    root.mkdir(parents=True)
    cur: pathlib.Path | None = None
    for i in range(count):
        if i % 1000 == 0:
            cur = root / f"{i // 1000:06d}"
            cur.mkdir()
        assert cur is not None
        (cur / f"{i % 1000:04d}.bin").write_bytes(f"{i:016x}\n".encode())
    marker.write_text("complete\n")


def gen_large(root: pathlib.Path, files: int, file_mib: int) -> None:
    marker = root.parent / f"{root.name}.complete-{files}x{file_mib}"
    if marker.exists():
        return
    if root.exists():
        shutil.rmtree(root)
    print(f">> generating {files} x {file_mib} MiB files in {root}", flush=True)
    root.mkdir(parents=True)
    template = os.urandom(file_mib * 1024 * 1024)
    for i in range(files):
        (root / f"{i:04d}.bin").write_bytes(template)
    marker.write_text("complete\n")


def tree_bytes(root: pathlib.Path) -> int:
    total = 0
    for base, _dirs, names in os.walk(root):
        for name in names:
            try:
                total += os.lstat(pathlib.Path(base) / name).st_size
            except OSError:
                pass
    return total


def verify_tree(src: pathlib.Path, dst: pathlib.Path) -> None:
    subprocess.check_call(
        ["diff", "-rq", "--exclude=.ripsync", str(src), str(dst)],
        stdout=subprocess.DEVNULL,
    )
    for base, dirs, files in os.walk(src, followlinks=False):
        rel_base = pathlib.Path(base).relative_to(src)
        dirs[:] = [d for d in dirs if d != ".ripsync"]
        for name in dirs + files:
            rel = rel_base / name
            a, b = os.lstat(src / rel), os.lstat(dst / rel)
            if stat.S_IFMT(a.st_mode) != stat.S_IFMT(b.st_mode):
                raise SystemExit(f"type mismatch: {rel}")
            if stat.S_IMODE(a.st_mode) != stat.S_IMODE(b.st_mode):
                raise SystemExit(f"mode mismatch: {rel}")
            if abs(a.st_mtime_ns - b.st_mtime_ns) > 1_000_000_000:
                raise SystemExit(f"mtime mismatch: {rel}")
            if stat.S_ISLNK(a.st_mode) and os.readlink(src / rel) != os.readlink(
                dst / rel
            ):
                raise SystemExit(f"symlink mismatch: {rel}")


# tool key -> argv builder taking (src, dst)
def ripsync_cmd(reflink: str):
    def build(src: pathlib.Path, dst: pathlib.Path) -> list[str]:
        return [str(RIPSYNC), str(src), str(dst), "--no-tui", "-q", "--reflink", reflink]

    return build


def rsync_cmd(src: pathlib.Path, dst: pathlib.Path) -> list[str]:
    return [RSYNC, "-a", f"{src}/", str(dst)]


TOOLS = [
    ("ripsync-cow", ripsync_cmd("auto")),
    ("ripsync-nocow", ripsync_cmd("never")),
    ("rsync", rsync_cmd),
]


def time_cmd(argv: list[str]) -> float:
    start = time.perf_counter()
    subprocess.check_call(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return time.perf_counter() - start


def record(row: dict[str, str | float | int]) -> None:
    fps = row["files"] / row["seconds"] if row["seconds"] else 0.0
    gibs = (row["bytes"] / (1024**3) / row["seconds"]) if row["seconds"] else 0.0
    line = (
        f'{row["cache"]},{row["scenario"]},{row["tool"]},{row["seconds"]:.6f},'
        f'{row["files"]},{row["bytes"]},{fps:.2f},{gibs:.6f},{row["fs"]}\n'
    )
    with open(RESULTS, "a", encoding="utf-8") as handle:
        handle.write(line)


def mutate(src: pathlib.Path, marker: str, count: int) -> None:
    paths = sorted(src.glob("*/*.bin"))[:count]
    for path in paths:
        path.write_text(f"changed-{marker}-{path.name}\n")


def bench_initial(cache: str, scenario: str, src: pathlib.Path, dest_root: pathlib.Path) -> None:
    files = sum(len(f) for _, _, f in os.walk(src))
    nbytes = tree_bytes(src)
    fs = fs_type(dest_root)
    for tool, build in TOOLS:
        for run in range(1, RUNS + 1):
            dst = dest_root / f"dst-{scenario}-{tool}-{run}"
            if dst.exists():
                shutil.rmtree(dst)
            if cache == "cold" and not drop_caches():
                sys.exit("cold-cache requested but dropping the page cache failed")
            seconds = time_cmd(build(src, dst))
            verify_tree(src, dst)
            record({"cache": cache, "scenario": scenario, "tool": tool,
                    "seconds": seconds, "files": files, "bytes": nbytes, "fs": fs})
            shutil.rmtree(dst)
            print(f"   {scenario:12} {tool:14} run {run:2}/{RUNS}  {seconds:.4f}s", flush=True)


def bench_resync(cache: str, src: pathlib.Path, dest_root: pathlib.Path) -> None:
    files = sum(len(f) for _, _, f in os.walk(src))
    nbytes = tree_bytes(src)
    fs = fs_type(dest_root)
    serial = 0
    for tool, build in TOOLS:
        for run in range(1, RUNS + 1):
            serial += 1
            dst = dest_root / f"dst-resync-{tool}-{run}"
            if dst.exists():
                shutil.rmtree(dst)
            time_cmd(build(src, dst))            # initial sync (untimed)
            mutate(src, str(serial), RESYNC_CHANGED)
            if cache == "cold" and not drop_caches():
                sys.exit("cold-cache requested but dropping the page cache failed")
            seconds = time_cmd(build(src, dst))  # incremental re-sync (timed)
            verify_tree(src, dst)
            record({"cache": cache, "scenario": "resync-changed", "tool": tool,
                    "seconds": seconds, "files": files, "bytes": nbytes, "fs": fs})
            shutil.rmtree(dst)
            print(f"   resync       {tool:14} run {run:2}/{RUNS}  {seconds:.4f}s", flush=True)


def main() -> int:
    if not RIPSYNC.exists():
        sys.exit(f"ripsync binary not found at {RIPSYNC} (cargo build --release)")
    BENCH_ROOT.mkdir(parents=True, exist_ok=True)

    if os.environ.get("BENCH_APPEND") != "1" or not RESULTS.exists():
        RESULTS.write_text(
            "cache,scenario,tool,seconds,files,bytes,files_per_sec,gib_per_sec,filesystem\n"
        )

    if CACHE_MODE == "both":
        caches = ["warm", "cold"]
    elif CACHE_MODE in ("warm", "cold"):
        caches = [CACHE_MODE]
    else:
        sys.exit("CACHE_MODE must be warm, cold, or both")

    print(f"host:    {platform.platform()}  ({platform.machine()})")
    print(f"ripsync: {RIPSYNC}  rev {git_rev()}")
    print(f"rsync:   {RSYNC}  [{tool_version([RSYNC, '--version'])}]")
    print(f"fs:      {fs_type(BENCH_ROOT)}   runs/config: {RUNS}   cache: {CACHE_MODE}")

    tiny_small = BENCH_ROOT / "src-tiny-small"
    tiny_large = BENCH_ROOT / "src-tiny-large"
    large = BENCH_ROOT / "src-large"
    resync = BENCH_ROOT / "src-resync"

    if "tiny-small" in SCENARIOS:
        gen_tiny(tiny_small, TINY_SMALL_COUNT)
    if "tiny-large" in SCENARIOS:
        gen_tiny(tiny_large, TINY_LARGE_COUNT)
    if "large" in SCENARIOS:
        gen_large(large, LARGE_FILES, LARGE_FILE_MIB)
    if "resync" in SCENARIOS:
        gen_tiny(resync, RESYNC_COUNT)

    for cache in caches:
        if "tiny-small" in SCENARIOS:
            bench_initial(cache, "tiny-small", tiny_small, BENCH_ROOT)
        if "tiny-large" in SCENARIOS:
            bench_initial(cache, "tiny-large", tiny_large, BENCH_ROOT)
        if "large" in SCENARIOS:
            bench_initial(cache, "large", large, BENCH_ROOT)
        if "resync" in SCENARIOS:
            bench_resync(cache, resync, BENCH_ROOT)

    print(f"results: {RESULTS}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
