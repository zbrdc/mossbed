# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-12

### Added

- `preload` and `dump`: a run can be handed a RAM-filesystem snapshot to restore
  before it starts, and can return one afterwards, so a workflow can carry state
  between runs. Runs stay stateless unless a caller asks for it.
- `TARDIGRADE_RUNNER_DUMP_MAX` (default 1 MiB) caps an inline dump. An oversize
  snapshot is suppressed with a note on stderr while the run's own status, stdout
  and stderr come back intact, so a single result cannot balloon a reply. The
  ceiling is an operator setting, not a tool argument.

### Fixed

- Preload and dump temp files take sequence-unique names. Two dumps running
  concurrently in one process used to collide with "File exists".

## [0.1.0] - 2026-10-12

First release.

- MCP stdio server exposing one tool, `run_js`, over tardigrade's sandboxed V8.
- Fail-closed startup, with a per-run deadline, an address-space belt and an
  explicit heap cap. The heap cap is an operator setting, never a tool argument.
- Results report `status`, `clock`, `epoch_ms`, `exit_code`, `stdout`, `stderr`,
  `duration_ms`, `memory_cap_mb` and `heap_limit_mb`, so the time base and the
  caps actually applied are visible to the caller.
- The virtual clock can be pinned with `epoch_ms`, which keeps a run both dated
  correctly and reproducible.
- A recognised V8 abort gets a note naming the workaround instead of a bare
  backtrace.
- 32 tests across four suites: unit, integration, boundary and agent-facing.

### Known limitations

- The guest's Intl date APIs abort the process: a known tardigrade bug, listed
  in its own KNOWN_BUGS.md.
- Guest recursion stops near 10 500 frames, because V8 stack size belongs to the
  embedder and is not settable from here.
- The suites that need the tardigrade binary skip themselves when it is absent;
  set TARDIGRADE_REQUIRE_TESTS=1 to make that a failure.

[Unreleased]: https://github.com/zbrdc/tardigrade-runner/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/zbrdc/tardigrade-runner/releases/tag/v0.2.0
[0.1.0]: https://github.com/zbrdc/tardigrade-runner/releases/tag/v0.1.0

