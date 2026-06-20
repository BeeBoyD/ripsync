# Remote sync over SSH

ripsync can sync to or from a remote host over ssh, the same way rsync does.
Give a source or destination as `[user@]host:path`:

```sh
ripsync ./site/ user@web01:/var/www/site     # push
ripsync user@web01:/var/www/site ./backup    # pull
```

Exactly one side may be remote per run (push **or** pull); remote-to-remote is
not supported.

## How it works

The locally-invoked ripsync spawns your ssh client to run `ripsync --server` on
the far host:

```
ripsync  ──spawns──▶  ssh host ripsync --server
   │                            │
   └────── binary protocol over the ssh pipe ──────┘
```

Both peers are ripsync — the protocol is **not** wire-compatible with the real
rsync daemon. ripsync must therefore be installed and on `PATH` on the remote
host. Everything else about your ssh setup is reused as-is: keys, `ssh-agent`,
`~/.ssh/config` aliases, `known_hosts`, jump hosts, and ports.

Use `-e`/`--rsh` to customize the remote shell command (it is split on
whitespace):

```sh
ripsync ./data host:/srv -e "ssh -p 2222 -i ~/.ssh/deploy"
```

## Delta transfer and compression

Files that already exist on the receiving side transfer as **deltas**: the
receiver computes a rolling-checksum [`Signature`] of its copy, the sender diffs
the new file against it and sends only the changed blocks, and the receiver
reconstructs the result. New files transfer whole. Force whole-file transfer with
`--whole-file`/`-W`.

`--compress`/`-z` zstd-compresses whole-file payloads on the wire
(`--compress-level N`, 1–22). Deltas are mostly literal runs already and are sent
uncompressed.

## Bandwidth limiting

`--bwlimit RATE` throttles the local→remote byte rate with a token bucket. A bare
number is KiB/s (as in rsync); a `K`/`M`/`G` suffix multiplies by 1024/1024²/1024³:

```sh
ripsync ./data backup@nas:/pool -z --bwlimit 2M   # ~2 MiB/s
```

## Security model

The far-end `ripsync --server` confines **every** read, write, and delete to the
negotiated root: relative paths containing `..` are rejected, and the real parent
directory of each target is canonicalized and checked to lie within the root
before any operation — the same defense (against the rsync symlink path-traversal
bug class) used by local syncs. The protocol begins with a magic+version
handshake and refuses a peer running an incompatible protocol version.

## Limitations (1.0)

- Remote `--dry-run` is not yet supported.
- Remote `--delete` requires `--yes` (there is no interactive remote preview yet).
- Whole files are held in memory during transfer; extremely large *new* files are
  better suited to the delta path once a copy exists on the receiver.
- The live TUI is used for local runs; remote runs print line/stats output.

[`Signature`]: https://docs.rs/ripsync-core/latest/ripsync_core/delta/struct.Signature.html
