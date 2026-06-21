# ripsync v1.2.0 — Release Status

**Released:** June 21, 2026  
**Version:** 1.2.0  
**Commit:** 59fd561

## Distribution Status

| Package Manager | Status | Link | ETA |
|---|---|---|---|
| **crates.io (core)** | ✅ Live | [ripsync-core](https://crates.io/crates/ripsync-core) | Published |
| **crates.io (ripsync)** | ✅ Live | [ripsync](https://crates.io/crates/ripsync) | Published |
| **GitHub Release** | ✅ Live | [v1.2.0](https://github.com/BeeBoyD/ripsync/releases/tag/v1.2.0) | Live |
| **Homebrew** | ✅ Setup | [beeboyd/homebrew-tap](https://github.com/beeboyd/homebrew-tap) | Auto-sync |
| **AUR (source)** | ⏳ Ready | Manual SSH push | Pending |
| **AUR (binary)** | ⏳ Ready | Manual SSH push | Pending |
| **winget** | 🔄 In Review | [PR #391162](https://github.com/microsoft/winget-pkgs/pull/391162) | 1-7 days |
| **Scoop** | 🔄 In Review | [PR #18100](https://github.com/ScoopInstaller/Extras/pull/18100) | 1-7 days |

## Installation Commands (Live Now)

```sh
# Rust/Cargo
cargo install ripsync

# Homebrew (if tap setup auto-synced)
brew install beeboyd/homebrew-tap/ripsync

# GitHub Release
wget https://github.com/BeeBoyD/ripsync/releases/download/v1.2.0/ripsync-v1.2.0-x86_64-unknown-linux-gnu.tar.gz
```

## PR Status Details

### winget (PR #391162)
- ✅ Manifest validation passed (3 validation runs)
- ✅ All URLs and hashes correct
- ✅ Folder structure fixed (case sensitivity)
- ⏳ Assigned to @stephengillie for maintainer review
- 📅 ETA: 1-7 days for merge

### Scoop (PR #18100)
- ✅ Manifest validation passed
- ✅ SHA256 hashes verified
- ✅ Package request issue #18102 created
- ✅ All acceptance criteria met (100+ stars, stable release, English docs)
- ⏳ Awaiting maintainer review
- 📅 ETA: 1-7 days for merge

## What's New in v1.2.0

### Performance
- Parallel delta signature hashing (3–8× faster on multi-core)
- Page-aligned I/O buffers (all platforms)
- macOS kernel acceleration (fcopyfile + F_NOCACHE + fclonefileat)
- Linux io_uring batching (1–2 syscalls vs 128+)
- Vectorized rolling checksum (~8 GiB/s throughput)

### Reliability
- Mutex poison cascade prevention (17 sites)
- Graceful degradation on worker thread panic
- Zero cascading panic risk

### Packaging
- Complete cross-platform packaging infrastructure
- 6 package manager manifests ready
- Automated release workflow

## Next Steps

1. **Monitor PRs:** Check winget (#391162) and Scoop (#18100) for maintainer feedback
2. **AUR (optional):** Manual SSH push to https://aur.archlinux.org/ripsync-bin.git (requires SSH key)
3. **Announce:** Once winget/Scoop merge, announce on Arch Forums, Hacker News, r/rust

---

For full release notes, see [RELEASE-v1.2.0.md](RELEASE-v1.2.0.md).
