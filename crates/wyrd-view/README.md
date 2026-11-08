# wyrd-view

Wyrd's mount-free read view: the live view and conflict-version lookup.
Actual FUSE mounting composes this view in the daemon; the view itself
never learns how bytes arrive.

## What belongs here

- The **live view** (`src/view.rs`): the drive's current state as an
  ordinary read-only folder — lookup, readdir, open, read, stat
- **Conflict versions**: looking up the versions of a multi-head conflict
  as `name@N` siblings with ordinary file tools (arbitrary historical
  browsing and restore are v0.5 work)
- **Materialization**: remote-only paths are visible and open on demand.
  Absent bytes map to `FetchStatus` at the view boundary; the daemon's
  want registry blocks open/read with a bounded deadline (`EIO` on
  expiry) and fetches
- **Conflict surfacing in the filesystem**: conflicted paths appear as
  conflict nodes with union listings of both versions, read-only

## What does not belong here

Storage, format, and networking logic. This crate translates filesystem
operations into format-layer calls and renders format-layer state as
files. It never mounts (mounting is the daemon's composition) and never
fetches (fetching is the engine's plan).

## Status

The mount-free view and the daemon's read-only FUSE backend (`wyrd mount`)
are in place and under test, including open-fd stability across head
advancement. Write support behind the mount is pending; direct writes
exist only as the `test-fixtures` test path on `WyrdNode`
(`put_file`/`remove`, never enabled in production builds) — production
writes submit through the live mutation channel.