# Filters

ripsync chooses which entries to transfer with four flags. They compose into a
single ordered matcher; for any path, the **first matching rule wins**, and a
path that matches no rule is **included**.

| Flag | Meaning |
|---|---|
| `--exclude PAT` | Drop paths matching the glob (and everything beneath a matching directory). |
| `--include PAT` | Keep paths matching the glob. Checked before `--exclude`. |
| `--filter "+ PAT"` / `"- PAT"` | Ordered keep/drop rule, highest priority. |
| `--files-from FILE` | Transfer exactly the relative paths listed in `FILE`. |

## Precedence

Rules are evaluated highest priority first:

1. `--filter` rules, in the order you give them;
2. all `--include` patterns;
3. all `--exclude` patterns.

So the common idiom keeps only one file type:

```sh
ripsync src dst --include '*.rs' --exclude '*'
```

`main.rs` matches `--include '*.rs'` → kept. `notes.txt` falls through to
`--exclude '*'` → dropped.

When you need a specific order between keeps and drops, use `--filter`:

```sh
# Keep one log, drop the rest
ripsync src dst --filter '+ build/important.log' --filter '- *.log'
```

## Globs and directories

Each pattern matches at the root, nested anywhere in the tree, and everything
beneath a matching directory. So `--exclude node_modules` drops
`node_modules/`, `pkg/node_modules/`, and all of their contents.

Filtering applies to **files and symlinks**; directories are always traversed
so surviving files remain reachable. An excluded directory may still be created
(empty) at the destination.

## `--files-from`

`--files-from` takes an explicit allowlist — one source-relative path per line,
with blank lines and `#` comments ignored:

```sh
printf 'README.md\nsrc/main.rs\n' > list.txt
ripsync ./project ./backup --files-from list.txt
```

Only the listed files are transferred. This pairs well with `git diff --name-only`
or any tool that emits a path list.
