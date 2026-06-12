# AUR packages

Two packages are provided:

- **`ripsync`** — builds from the release source tarball
  (`packaging/aur/ripsync/`).
- **`ripsync-bin`** — installs the prebuilt cargo-dist Linux tarball
  (`packaging/aur/ripsync-bin/`); `provides`/`conflicts` `ripsync`.

## Publishing (per package)

1. Bump `pkgver` and set the real checksums:

   ```sh
   cd packaging/aur/ripsync           # or ripsync-bin
   updpkgsums                         # replaces the SKIP sha256sums
   makepkg --printsrcinfo > .SRCINFO
   ```

2. Test the build locally:

   ```sh
   makepkg -si
   ```

3. Push to the AUR (one git repo per package, remote
   `ssh://aur@aur.archlinux.org/<pkgname>.git`):

   ```sh
   git clone ssh://aur@aur.archlinux.org/ripsync.git aur-ripsync
   cp PKGBUILD .SRCINFO aur-ripsync/
   cd aur-ripsync && git add PKGBUILD .SRCINFO
   git commit -m "ripsync 0.4.0" && git push
   ```

`$pkgver` tracks the git tag; `ripsync-bin` consumes the cargo-dist
`*-unknown-linux-gnu.tar.gz` release assets. Keep the committed `.SRCINFO` in
sync with each `PKGBUILD` (CI / reviewers check this).
