# Watch mode

`--watch` turns ripsync into a continuous mirror: it runs once, then re-syncs
whenever the source tree changes.

```sh
ripsync ./site ./preview --watch
```

It runs an initial sync, then watches the source recursively. Filesystem events
are coalesced for the debounce window (`--debounce MS`, default 300 ms) so a
burst of edits collapses into a single sync. Each cycle reuses the persistent
index, so unchanged files are skipped cheaply — only what changed is copied.

```sh
# React faster (50 ms debounce)
ripsync ./src ./dst --watch --debounce 50
```

Press **Ctrl-C** to stop.

## Behaviour

- **Local only.** Watch does not yet drive remote (`host:path`) transfers.
- **Resilient.** A failed cycle is reported to stderr and the watch keeps
  running; the next change triggers another attempt.
- **Filters apply.** `--include`/`--exclude`/`--files-from` are honoured every
  cycle, exactly as in a one-shot run.
- **Self-writes ignored.** Changes under ripsync's own `.ripsync/` metadata
  directory do not retrigger a sync.

## Example: live-deploy a build directory

```sh
ripsync ./dist deploy@web01:/var/www/site   # one-shot remote publish
ripsync ./dist ./staging --watch            # or mirror locally on every save
```
