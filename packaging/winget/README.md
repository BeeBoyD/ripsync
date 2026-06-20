# Windows package managers

## Scoop

`packaging/scoop/ripsync.json` installs the cargo-dist Windows zip. Publish it to
a Scoop bucket (e.g. `beeboyd/scoop-bucket`):

```sh
# In the bucket repo:
cp packaging/scoop/ripsync.json bucket/ripsync.json
# Fill the hash from the release zip:
#   (Get-FileHash ripsync-v1.0.0-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash
git commit -am "ripsync 1.0.0" && git push
```

Users then:

```powershell
scoop bucket add beeboyd https://github.com/beeboyd/scoop-bucket
scoop install ripsync
```

`checkver` + `autoupdate` let `scoop update` track new GitHub releases; run
`scoop-checkver -u ripsync` in the bucket to refresh the hash automatically.

## winget

`packaging/winget/` holds the three-file manifest set for the MSI:

- `beeboyd.ripsync.yaml` — version manifest
- `beeboyd.ripsync.installer.yaml` — installer (wix/MSI, x64)
- `beeboyd.ripsync.locale.en-US.yaml` — locale/metadata

### Publishing to the winget community repo

1. Fill `InstallerSha256` and `ProductCode` from the released MSI. The fastest
   path is `wingetcreate update beeboyd.ripsync --version 1.0.0 --urls <MSI-URL>`
   which computes the hash and product code for you.
2. Validate: `winget validate --manifest packaging/winget` and, in a sandbox,
   `winget install --manifest packaging/winget`.
3. Open a PR to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)
   placing the files under
   `manifests/b/beeboyd/ripsync/1.0.0/`. `wingetcreate submit` automates the PR.

Once merged: `winget install beeboyd.ripsync`.

The committed manifests use placeholder hashes / product code (all-zero) — these
must be replaced with the real values before submission.
