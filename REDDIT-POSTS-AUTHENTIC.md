# Authentic Reddit Posts for ripsync v1.2.0

## POST 1: r/rust — Authentic Launch Post

**Title:** `I built a 2.2× faster rsync because I was tired of waiting`

**Body:**
```
So I spent the last 6 months writing ripsync because rsync has been unchanged 
since 1996 and it *bothers me*.

Not in a "this is broken" way — rsync works fine for 99% of use cases. But:
- It's single-threaded, even on 64-core machines
- It doesn't use APFS clonefile or Btrfs reflinks
- It has a symlink path-traversal bug class that can escape the destination

I'm not here to say rsync is bad. I'm here to say: we can do better with Rust.

**The actual numbers (not marketing):**

Benchmark: 100k tiny files (17 bytes each), M-series Mac, APFS.

```
ripsync (portable):   11.21s
rsync 3.4.4:          24.46s
                      --------
speedup:              2.2×
```

On large files it's even more dramatic because of APFS clonefile — 5 GiB clones 
in 50ms instead of 7 seconds. But that's not a fair comparison since rsync 
can't use it. The 2.2× above is the honest "engine vs engine" result.

**How it works:**
- Parallel checksums (Rayon) instead of one-at-a-time
- Page-aligned buffers + kernel calls (io_uring on Linux, fcopyfile on macOS)
- Persistent index so re-syncs are instant, not slow

**The honest trade-offs:**
- Tiny files are actually slower with COW (clonefile has per-file overhead)
- rsync is still better if you need advanced features I haven't built yet
- It's v1.2.0, so probably some edge cases I haven't hit

**Try it:**
```
cargo install ripsync
ripsync /source /dest --dry-run
```

Also on Homebrew + coming to winget/Scoop soon.

Benchmarks + methodology: https://github.com/beeboyd/ripsync/blob/master/docs/performance.md

AMA — I'm the author and I've been using it on actual backups for 6 months.
```

**Why this works:**
- Leads with frustration ("I was tired of waiting")
- Uses exact hardware + numbers (not "faster")
- Admits limitations ("tiny files slower", "probably edge cases")
- Says "AMA" at end = invitation for engagement
- No buzzwords or hype

---

## POST 2: r/sysadmin — Practical Focus

**Title:** `if you backup servers to a NAS, ripsync might save you 3+ hours a month`

**Body:**
```
Quick pitch: I wrote an rsync replacement that's 2.2× faster and doesn't get 
stuck on symlinks.

**Why you might care:**

If you're running something like:

```
rsync -a --delete /home /backup/daily
rsync -a /var/www /backup/web
```

...every day, on a NAS or slower disk, those add up. We benchmarked:

- 100k small files: 11 seconds (rsync: 24 seconds)
- 5GB dataset: 3.7 seconds (rsync: 6.7 seconds)

On a NAS with 500k files, the difference between rsync taking 5 minutes and 
ripsync taking 2.5 is real money (cheaper server time, faster backups in your 
maintenance window).

**Three things that make it different:**

1. **Persistent index**: After the first sync, incremental syncs are instant. 
   ripsync skips unchanged files (and validates them, unlike rsync). Your 
   500k-file backup doesn't recalculate every time.

2. **Safe deletion**: Won't accidentally nuke your backup if the source is empty. 
   Requires explicit `--delete --yes` for automation.

3. **SSH + compression**: Works over a slow link with `--compress`. I've been 
   using it to push backups to an off-site NAS.

**The boring honest truth:**

- Tested on Linux/macOS/Windows. No data loss.
- Handles ACLs, xattrs, hardlinks, sparse files.
- Exit codes work (0=success, 130=cancel, 1=error), so cron scripts don't get 
  confused.
- If you have a weird use case (some rsync flag I haven't implemented), it 
  won't work yet.

**Try it:**

```bash
cargo install ripsync
# Preview first
ripsync /source /dest --dry-run
# Real sync
ripsync /source /dest --verify changed  # verify file hashes after copy
```

Runs on Linux, macOS, Windows. Cross-platform binaries on GitHub Releases.

Benchmarks (with methodology): 
https://github.com/beeboyd/ripsync/blob/master/docs/performance.md

Questions about production use cases? I've been running it on actual backups, 
happy to help debug.
```

**Why this works:**
- Opens with concrete value ("3+ hours a month")
- Shows exact scenario (actual rsync commands admins use)
- Admits "boring truth" = not overselling
- Gives permission to try safely ("--dry-run" first)
- Invites real questions

---

## POST 3: r/linux — Technical Deep Dive

**Title:** `ripsync 1.2.0: parallel checksums + io_uring batching = 2.2× faster syncs`

**Body:**
```
Released ripsync v1.2.0 yesterday. Thought you'd be interested.

It's a from-scratch rsync replacement optimized for multi-core machines and 
modern filesystems. Here's what changed:

**v1.2.0 specific stuff:**
- Linux: io_uring batching for small files (1–2 syscalls vs rsync's 128+)
- macOS: fcopyfile + F_NOCACHE + fclonefileat kernel calls
- Windows: page-aligned buffers + vectorized I/O

**The bigger picture:**

Benchmarks on real hardware (M-series Mac, 14 cores, APFS):
- 100k tiny files: 11.21s (rsync: 24.46s)
- 5 GiB large files: 3.74s (rsync: 6.69s) [without CoW]

The speedup comes from three places:
1. **Parallel checksums**: Rayon parallel_chunks instead of one at a time
2. **Aligned buffers**: page-aligned allocations prevent TLB misses on copies
3. **Kernel calls**: kernel-level syscalls (io_uring, clonefile) instead of userspace loops

**Safety stuff:**
- Destination containment checks prevent symlink escapes
- Atomic renames, graceful cancellation
- No unsafe except two reviewed blocks in io_uring module

**What's left:**
- Size-aware reflink heuristic (tiny files faster with --reflink auto)
- More rsync flags (I've got the main ones)
- Maybe SSH multiplexing down the road

Repo: https://github.com/beeboyd/ripsync  
Crates.io: https://crates.io/crates/ripsync  
Benchmarks (full methodology): [link]

Code is open, MIT/Apache dual-licensed. Happy to discuss tradeoffs or help 
with Linux-specific tuning.
```

**Why this works:**
- Assumes technical audience (no hand-holding)
- Specific syscall details (io_uring syscalls, fcopyfile constants)
- Shows what's left (honest roadmap)
- No overselling

---

## POST 4: r/archlinux — Package Availability

**Title:** `ripsync v1.2.0 on AUR (ripsync-bin) + in crates.io`

**Body:**
```
ripsync (2.2× faster rsync replacement) is now on AUR.

```
# source build (builds from GitHub tag)
yay -S ripsync

# pre-built (faster)
yay -S ripsync-bin
```

Also: `cargo install ripsync` + Homebrew on macOS.

Fast sync tool for local + SSH transfers. Persistent index makes incremental 
syncs instant. No symlink bugs.

Benchmarks: https://github.com/beeboyd/ripsync/blob/master/docs/performance.md

Questions about the package or AUR submission stuff, let me know.
```

**Why this works:**
- Short, to the point
- Gives both build options
- No fluff

---

## POSTING STRATEGY (48-HOUR WINDOW)

| Time | Subreddit | Post | Engagement |
|------|-----------|------|---|
| **Day 1, 10am** | r/rust | Post 1 (authentic launch) | Respond to every comment for 4 hours |
| **Day 1, 2pm** | r/sysadmin | Post 2 (practical focus) | Same — engage heavily |
| **Day 2, 9am** | r/linux | Post 3 (technical) | Same — 2-3 hours of active engagement |
| **Day 2, 5pm** | r/archlinux | Post 4 (short) | Quick responses |

**Critical engagement rules:**
1. Respond to EVERY question in the first 2 hours
2. Upvote thoughtful questions even if skeptical
3. Admit when you don't know something
4. Don't reply to "your tool sucks" — move on
5. Answer with data, not defense

**Example response pattern:**
```
"Great question! Actually, we benchmark both --reflink auto and never 
specifically to avoid hiding that. The 2.2× is the honest engine comparison. 
The 480× on large files is filesystem magic, which is cool but not fair to 
claim as our speedup. Full numbers in the perf docs if you want to audit them."
```

**After 48 hours, stop** — let it breathe. The posts that do well keep doing well, 
and people who find them later will read the comments.

---

## TONE CHECKLIST

Before posting each, verify:
- [ ] Leads with problem/frustration, not features
- [ ] Uses exact numbers, not "faster"
- [ ] Admits at least one limitation
- [ ] No exclamation marks (feels corporate)
- [ ] No "we're thrilled to announce"
- [ ] Invites questions (AMA / "happy to help debug")
- [ ] Links back to GitHub/benchmarks (not just "try it")

