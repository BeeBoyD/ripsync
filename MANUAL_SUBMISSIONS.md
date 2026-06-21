# Manual Package Manager Submissions for ripsync v1.2.0

## Quick Summary

After release, these package managers need **manual submission**:

| PM | Time to availability | Difficulty | Link |
|---|---|---|---|
| **AUR** | ~1 hour | Easy (SSH push) | https://aur.archlinux.org/packages/ripsync-bin |
| **winget** | 24-48 hours | Medium (PR to Microsoft) | https://github.com/microsoft/winget-pkgs |
| **Scoop** | 1-7 days | Medium (PR to Scoop) | https://github.com/lukesampson/scoop-extras |

---

## 1. AUR (Arch Linux) — RECOMMENDED FIRST

**Status:** Fastest path to users (1 hour). Pre-built binary package.

### Prerequisites
- SSH key configured for `aur.archlinux.org`
- Arch Linux AUR account

### One-liner (copy & paste)
```bash
git clone ssh://aur@aur.archlinux.org/ripsync-bin.git /tmp/aur && \
cd /tmp/aur && \
cp /Users/beeboyd/Developer/ripsync/packaging/aur/ripsync-bin/PKGBUILD . && \
cp /Users/beeboyd/Developer/ripsync/packaging/aur/ripsync-bin/.SRCINFO . && \
git add . && git commit -m "Update to v1.2.0: platform-optimized performance" && \
git push origin master
```

### Step-by-step
```bash
# 1. Clone
git clone ssh://aur@aur.archlinux.org/ripsync-bin.git /tmp/ripsync-bin-aur
cd /tmp/ripsync-bin-aur

# 2. Copy files
cp /Users/beeboyd/Developer/ripsync/packaging/aur/ripsync-bin/PKGBUILD .
cp /Users/beeboyd/Developer/ripsync/packaging/aur/ripsync-bin/.SRCINFO .

# 3. Verify
cat PKGBUILD | grep pkgver
# Should show: pkgver=1.2.0

# 4. Commit & push
git add PKGBUILD .SRCINFO
git commit -m "Update to v1.2.0: platform-optimized performance"
git push origin master

# 5. Verify on AUR
# Visit: https://aur.archlinux.org/packages/ripsync-bin
# Should show v1.2.0 within 2-5 minutes
```

### Verify installation
```bash
yay -S ripsync-bin
ripsync --version
```

---

## 2. winget (Windows Package Manager)

**Status:** Requires PR approval (24-48 hours). Windows primary package manager.

### Prerequisites
- GitHub account
- Git installed

### Steps

```bash
# 1. Fork https://github.com/microsoft/winget-pkgs (web UI)

# 2. Clone your fork
git clone https://github.com/<YOUR_USERNAME>/winget-pkgs.git
cd winget-pkgs

# 3. Add upstream
git remote add upstream https://github.com/microsoft/winget-pkgs.git
git fetch upstream

# 4. Create branch
git checkout -b ripsync-v1.2.0 upstream/master

# 5. Create manifest directory
mkdir -p manifests/b/beeboyd/ripsync/1.2.0
cd manifests/b/beeboyd/ripsync/1.2.0

# 6. Copy manifest files
cp /Users/beeboyd/Developer/ripsync/packaging/winget/beeboyd.ripsync.yaml .
cp /Users/beeboyd/Developer/ripsync/packaging/winget/beeboyd.ripsync.installer.yaml .
cp /Users/beeboyd/Developer/ripsync/packaging/winget/beeboyd.ripsync.locale.en-US.yaml .

# 7. Verify files
grep "PackageVersion:" beeboyd.ripsync.yaml
# Should show: PackageVersion: 1.2.0

grep "InstallerSha256:" beeboyd.ripsync.installer.yaml
# Should show SHA256 hashes

# 8. Commit & push from root
cd /path/to/winget-pkgs
git add manifests/b/beeboyd/ripsync/1.2.0/
git commit -m "Add ripsync v1.2.0: platform-optimized performance"
git push origin ripsync-v1.2.0

# 9. Create PR on GitHub (web UI)
# Visit: https://github.com/<YOUR_USERNAME>/winget-pkgs
# Click "Compare & pull request"
# Title: "Add ripsync v1.2.0"
# Description: "Add package for ripsync v1.2.0 - platform-optimized performance"
```

### Verify installation
```bash
winget install beeboyd.ripsync
ripsync --version
```

---

## 3. Scoop (Windows Package Manager) — Optional

**Status:** Optional alternative to winget. Requires PR approval (1-7 days).

### Prerequisites
- GitHub account
- Git installed
- (Optional) Scoop installed for local testing

### Steps

```bash
# 1. Fork https://github.com/lukesampson/scoop-extras (web UI)

# 2. Clone your fork
git clone https://github.com/<YOUR_USERNAME>/scoop-extras.git
cd scoop-extras

# 3. Create branch
git checkout -b ripsync-v1.2.0

# 4. Copy manifest
cp /Users/beeboyd/Developer/ripsync/packaging/scoop/ripsync.json bucket/ripsync.json

# 5. Verify file
cat bucket/ripsync.json | grep version
# Should show: "version": "1.2.0"

# 6. (Optional) Test locally
scoop install bucket/ripsync.json
ripsync --version

# 7. Commit & push
git add bucket/ripsync.json
git commit -m "Add ripsync v1.2.0: platform-optimized performance"
git push origin ripsync-v1.2.0

# 8. Create PR on GitHub (web UI)
# Visit: https://github.com/<YOUR_USERNAME>/scoop-extras
# Click "Compare & pull request"
# Title: "Add ripsync v1.2.0"
```

### Verify installation
```bash
scoop install ripsync
ripsync --version
```

---

## Verification Checklist

Before each submission, verify:

- ✅ Version number is `1.2.0`
- ✅ SHA256 checksums match `.sha256` files from GitHub Release
- ✅ URLs point to `https://github.com/BeeBoyD/ripsync/releases/download/v1.2.0/`
- ✅ Files are properly formatted (YAML/JSON)
- ✅ Commit message is descriptive
- ✅ Branch name is clear (e.g., `ripsync-v1.2.0`)
- ✅ PR description explains what's being added

---

## Reference Links

### Files Location
- Local: `/Users/beeboyd/Developer/ripsync/packaging/`
- AUR: `aur/ripsync-bin/`
- winget: `winget/`
- Scoop: `scoop/`

### GitHub
- GitHub Release: https://github.com/BeeBoyD/ripsync/releases/tag/v1.2.0
- Homebrew Tap: https://github.com/beeboyd/homebrew-tap

### Package Manager Docs
- AUR: https://wiki.archlinux.org/title/AUR
- winget: https://github.com/microsoft/winget-pkgs/blob/master/CONTRIBUTING.md
- Scoop: https://github.com/lukesampson/scoop/wiki/App-Manifests

---

## Timeline

| PM | Submit | Available |
|---|---|---|
| AUR | Now | ~1 hour |
| winget | Now | 24-48 hours (after PR merge) |
| Scoop | Now | 1-7 days (after PR merge) |

---

## Already Live (No Action Needed)

- ✅ **GitHub Release:** https://github.com/BeeBoyD/ripsync/releases/tag/v1.2.0
- ✅ **Homebrew:** `brew install beeboyd/tap/ripsync`
- ✅ **crates.io:** `cargo install ripsync`
