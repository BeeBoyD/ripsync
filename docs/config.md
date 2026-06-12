# Configuration file

ripsync reads an optional config file that supplies **defaults** for a few flags.
The command line always overrides it; the file never forces behavior.

## Location

| Platform | Path |
|---|---|
| Linux / macOS | `$XDG_CONFIG_HOME/ripsync/config.toml`, else `~/.config/ripsync/config.toml` |
| Windows | `%APPDATA%\ripsync\config.toml` |

A missing file is ignored; a malformed file is reported as a warning and ignored.

## Keys

```toml
reflink = "always"            # auto | always | never
fsync   = "never"             # auto | always | never
backend = "uring"             # auto | uring | portable
threads = 8                   # worker threads
exclude = ["*.tmp", "node_modules", "*.log"]
```

Only these keys are honored (unknown keys are rejected so typos surface). This is
**not** a profiles mechanism — remote profiles remain out of scope.

## Precedence

For each setting: an explicit command-line flag wins; otherwise the config value
applies; otherwise ripsync's built-in default. For example, with `backend =
"uring"` in the file, `ripsync SRC DST` uses uring, but `ripsync SRC DST
--backend portable` uses portable.
