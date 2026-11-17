# 3.0 migration notes

Use the [2.x to 3.0 migration guide](2.0-to-3.0.md) for API, behavior and
deployment changes. The [migration index](README.md) links earlier upgrades.

## Restarting a recorder after incomplete file tracking

Native event loss, unreadable lifecycle evidence or a file-bookkeeping limit
now preserves an unknown cleanup obligation. Previously, a recorder could start
another session once every known file closed, even after a lost open event.
Both `FileRecorder::start` and `AsyncFileRecorder::start` now return
`InvalidState(StateConflict::PreviousSessionOpen)` until a successful `reset`
replaces that native recorder. Reset even if every observed file closed and
the SDK stopped recording; those observations cannot account for hidden files.

Replace same-recorder restart after incomplete tracking with `reset(&options)`
(await it for async handles), inspect the returned report, then call `start`.
A failed reset preserves the old recorder and its cleanup obligation. Reset
abandons any unfinished files and can leave truncated output; `abandon` remains
available for immediate teardown without creating a replacement.

Drop and async `close` now consume the remaining original `drop_wait` budget
when tracking is incomplete, including after all known files close. They do not
restart that budget or change an already-final indeterminate report. Normal
sessions with complete tracking still finish cleanup early after all files close.
Async application-notification eviction alone does not make tracking incomplete.
