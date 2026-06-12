# APT repository for ripsync

This directory documents how to publish ripsync `.deb` packages as a signed APT
repository (on GitHub Pages or beeboyd.com). The live repository is **not** stood
up automatically — follow these steps to host one.

## 1. Build the package

```sh
cargo install cargo-deb
./packaging/deb/build.sh        # -> target/debian/ripsync_<ver>_<arch>.deb
```

## 2. Create a signing key (once)

```sh
gpg --quick-generate-key "ripsync apt <ionescudan888@gmail.com>" rsa4096 sign 0
gpg --armor --export "ripsync apt" > ripsync-archive-keyring.asc
```

Keep the secret key offline / in CI secrets; publish only the public `.asc`.

## 3. Build the repo with aptly or reprepro

Using `reprepro` (simple, file-based; good for Pages):

```sh
mkdir -p apt-repo/conf
cat > apt-repo/conf/distributions <<EOF
Origin: ripsync
Label: ripsync
Codename: stable
Architectures: amd64 arm64
Components: main
Description: ripsync APT repository
SignWith: <YOUR_KEY_ID>
EOF

reprepro -b apt-repo includedeb stable target/debian/ripsync_*.deb
cp ripsync-archive-keyring.asc apt-repo/
```

Publish the `apt-repo/` tree to GitHub Pages (or `beeboyd.com/apt`).
`packaging/apt/publish.sh` automates steps 1 and 3.

## 4. What users add

```sh
curl -fsSL https://beeboyd.github.io/ripsync/ripsync-archive-keyring.asc \
  | sudo tee /usr/share/keyrings/ripsync-archive-keyring.asc >/dev/null

echo "deb [signed-by=/usr/share/keyrings/ripsync-archive-keyring.asc] \
https://beeboyd.github.io/ripsync stable main" \
  | sudo tee /etc/apt/sources.list.d/ripsync.list

sudo apt-get update
sudo apt-get install ripsync
```

## CI

`.github/workflows/apt.yml` is a **stub**: it builds the `.deb` and uploads it as
a workflow artifact. It deliberately does not sign or publish to a live repo —
wire in your `GPG_PRIVATE_KEY` secret and a Pages deploy step when you are ready.
