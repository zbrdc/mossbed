# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-10-17

### Changed

- **Renamed: project, crate and binary are now `mossbed`** (was
  `tardigrade-runner` — after the moss a tardigrade lives in, since this
  server is the habitat the engine runs in). Every operator setting renamed
  with it: `TARDIGRADE_RUNNER_*` is now `MOSSBED_*`. Unchanged: tool names
  keep their `tardigrade_` prefix because they operate the tardigrade
  engine, and `TARDIGRADE_BIN` still selects that engine binary.

### Added

- Sessions: `session_open`/`session_eval`/`session_close` over tardigrade's
  `--serve` console mode, listed only when the binary offers it. A session is
  one guest whose globals, timers and RAM files persist across one-line
  expression evals, under a mandatory per-command timeout and whole-session
  TTL. `MOSSBED_MAX_SESSIONS` (default 8) and
  `MOSSBED_SESSION_IDLE_MS` (default 300000) bound them.
- Operator-granted network capabilities: `MOSSBED_ALLOW_CALL`
  (`NAME=URL[,METHOD]`, `;`-separated) lets guests call named endpoints with
  `vm.net.request`; without it guests are fully offline, as before. Byte
  budgets and per-request timeout are operator settings too, private-address
  endpoints need explicit `MOSSBED_NET_ALLOW_PRIVATE=1`, and
  results/tool descriptions report grant names only — never URLs.
- Concurrency and cancellation: requests dispatch on threads, and
  `notifications/cancelled` SIGKILLs the named run, reported as
  `status: "cancelled"`. Tool results now also carry `structuredContent`,
  and `initialize` negotiates among protocol versions 2024-11-05,
  2025-03-26 and 2025-06-18.
- `run_js` gained `args` (guest `vm.args`) and `module` (ES-module entry)
  parameters, and `seed` now spans tardigrade's full u64 range.
- `MOSSBED_FS_QUOTA` (default 64 MiB) passes `--fs-quota`
  explicitly; results report `fs_quota_mb` and `net_capabilities`.
- `MOSSBED_INPUT_MAX` (default 4 MiB) caps inbound `code` and
  `preload`, matching the outbound caps that always existed.
- Binary resolution falls back to `tardigrade` on `PATH` when the sibling
  checkout is absent, and startup now probes the flags the binary
  advertises, refusing an incompatible one by name.

### Fixed

- The address-space belt is raised to cover V8's own reservation. Tardigrade's
  current build (rusty_v8 139 with pointer compression) reserves about 69 GiB
  of address space before any guest byte, so the old 2 GiB belt aborted V8 at
  startup — every run failed, whatever the guest did. `MOSSBED_AS`
  is now a floor and the effective belt is that floor plus the heap cap.
- The OOM abort note hardcoded the 256 MiB heap default; it now names the
  cap actually applied.
- Tests and the Intl failure note tracked a tardigrade that aborted inside
  ICU. Current tardigrade stubs Intl to a catchable error (exit 1), counts
  WebAssembly memory against `--memory`, and stops a Uint8Array spree at
  128 of the 256 MiB budget; the four affected tests now assert the
  boundary properties rather than those incidental numbers, and the README
  points at tardigrade's `docs/` paths.

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
[0.3.0]: https://github.com/zbrdc/tardigrade-runner/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/zbrdc/tardigrade-runner/releases/tag/v0.2.0
[0.1.0]: https://github.com/zbrdc/tardigrade-runner/releases/tag/v0.1.0

