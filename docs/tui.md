# TUI

The dashboard opens before planning and tracks the complete lifecycle. Its
header shows phase, selected backend, verification mode, thread count, and
destructive/dry-run options. Entry and byte gauges are followed by throughput,
files per second, elapsed time, error counts, and active operations.

Views:

- Summary: counts, verification progress, backend reason, active operations.
- Activity: copy, update, directory, and link events.
- Deletes: the complete deletion review list.
- Errors/Verify: operation errors and verification mismatches.

Filtering is a substring match on the current list. `f` cycles all, activity,
delete, error, and verification events. The layout removes the metrics row on
small terminals. Set `NO_COLOR` to suppress color.

`p` controls engine work, not just rendering. `q` and `Ctrl-C` open a
confirmation overlay; confirmation requests cooperative cancellation. Terminal
raw mode, alternate screen, cursor state, and error cleanup are owned by an
RAII guard.
