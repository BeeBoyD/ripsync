# docs/assets

`ripsync-demo.gif` is generated from `../demo.tape` with
[VHS](https://github.com/charmbracelet/vhs):

```sh
cargo build --release
vhs docs/demo.tape   # writes docs/assets/ripsync-demo.gif
```

Regenerate and commit the GIF when the TUI changes. It is embedded at the top of
the repository README and the docs-site introduction.
