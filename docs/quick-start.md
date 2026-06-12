# Quick start

```sh
# Mirror SOURCE into DESTINATION (TUI starts automatically on a terminal).
ripsync SOURCE DESTINATION

# Preview without changing the destination.
ripsync SOURCE DESTINATION --dry-run

# Mirror deletions interactively (type DELETE to approve).
ripsync SOURCE DESTINATION --delete

# Automation must approve deletion explicitly.
ripsync SOURCE DESTINATION --delete --yes --no-tui

# Hash changed files after copying, or compare complete trees.
ripsync SOURCE DESTINATION --verify changed
ripsync SOURCE DESTINATION --verify all

# Machine-readable summary for pipelines.
ripsync SOURCE DESTINATION --no-tui --output json
ripsync SOURCE DESTINATION --no-tui --stats
```

Use `--no-tui`, `--output json`, or `--quiet` for noninteractive operation;
`NO_COLOR` and piping are honored. Run `ripsync --help` for the full flag list,
or read the generated [man page](#man-page-and-completions).

## Man page and completions

```sh
ripsync _gen man > ripsync.1
ripsync _gen completions bash   # or zsh | fish | powershell
```

Packages install these automatically. See [installation](install.md).
