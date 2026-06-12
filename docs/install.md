# Installation

| Method | Command | Platforms |
|---|---|---|
| Homebrew | `brew install beeboyd/tap/ripsync` | macOS, Linux |
| apt (.deb) | `sudo apt-get install ripsync` (after adding the repo) | Debian/Ubuntu |
| AUR | `paru -S ripsync` (source) or `paru -S ripsync-bin` | Arch |
| Scoop | `scoop install ripsync` (after `scoop bucket add beeboyd …`) | Windows |
| winget | `winget install beeboyd.ripsync` | Windows |
| MSI | download from the GitHub Release | Windows |
| Shell installer | `curl -fsSL …/ripsync-installer.sh \| sh` | macOS, Linux |
| PowerShell installer | `irm …/ripsync-installer.ps1 \| iex` | Windows |
| cargo | `cargo install ripsync` | any with a Rust toolchain |
| from source | `cargo build --release` → `target/release/ripsync` | any |

The shell/PowerShell installers, Homebrew formula, and MSI are produced by
cargo-dist on each `v*` tag (see [releasing](releasing.md)). apt/AUR/Scoop/winget
recipes live under `packaging/`.

## Short alias `rs`

An optional `rs` alias (same program) can be built with
`cargo build --features rs-alias`. It is **off by default** and packagers install
it only where it does not collide with the BSD `rs` reshape utility:

| Manager | `rs` alias |
|---|---|
| cargo / source | opt-in via `--features rs-alias` |
| Homebrew, apt, AUR | not installed by default (would conflict with `rs`/util-linux) |
| Scoop, winget | not installed by default |

## Build features

- `--no-default-features` drops mimalloc (uses the system allocator).
- `--features system-malloc` forces the system allocator while keeping defaults.
