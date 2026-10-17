# Changelog

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project uses [semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Documented JavaScript and WebAssembly execution across README, language guide,
  package/API summaries, and MCP tool descriptions. TypeScript needs transpilation.
  C/C++/Rust need WebAssembly compilation and compatible imports.
- Clarified host-build isolation, WASI requirements, and WebAssembly memory limits.
- Replaced the old line-art logo with the moss tile icon and sheltered-tardigrade
  mascot. Added transparent SVG/PNG assets, a favicon, and usage guidance.
- Updated installation and migration instructions for the mossbed name.
- Made README setup instructions client-neutral; moved rename migration notes here.
- Renamed the GitHub repository to `zbrdc/mossbed` and updated repository links.
- **Renamed: project, crate and binary are now `mossbed`** (was
  `tardigrade-runner` — after the moss a tardigrade lives in, since this
  server is the habitat the engine runs in). Every operator setting renamed
  with it: `TARDIGRADE_RUNNER_*` is now `MOSSBED_*`. The unprefixed MCP tool
  names remain `run_js`, `session_open`, `session_eval`, and `session_close`.
  Client-generated prefixes depend on the MCP registration name.
  `TARDIGRADE_BIN` still selects the engine binary, and
  `TARDIGRADE_ALLOW_UNCAPPED` keeps its existing name.

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

- The address-space limit covers V8's startup reservations. With rusty_v8 152,
  the former 72 GiB floor could abort the Oilpan cage reservation.
  The effective limit is the larger of `MOSSBED_AS` and 128 GiB plus the heap cap.
  This is virtual address space, not resident memory.
- The OOM abort note hardcoded the 256 MiB heap default; it now names the
  cap actually applied.
- Tests and the Intl failure note tracked a tardigrade that aborted inside
  ICU. Current tardigrade stubs Intl to a catchable error (exit 1), counts
  JavaScript-created `WebAssembly.Memory` reservations against `--memory`,
  and stops a Uint8Array spree at 128 of the 256 MiB budget. The four affected
  tests now assert the boundary properties rather than those incidental numbers, and the README
  points at tardigrade's `docs/` paths.

### Upgrade from tardigrade-runner

Mossbed is the new name for `tardigrade-runner`. The separate `tardigrade`
runtime keeps its name. The repository is now `zbrdc/mossbed`.

For an existing clone, update its remote:

    git remote set-url origin https://github.com/zbrdc/mossbed.git

- Update the MCP command path to the `mossbed` binary in your actual checkout.
- Replace each `TARDIGRADE_RUNNER_*` setting with its `MOSSBED_*` equivalent.
  The old names are no longer read. Migrate custom limits and grants before
  starting the new binary.
- Keep `TARDIGRADE_BIN` and `--tardigrade=<path>` unchanged.
- Use `mossbed` as the MCP registration name for new installations.
  Existing installations can retain their registration name to preserve
  client-generated tool prefixes.
- Reconnect the MCP server after updating its configuration.

The unprefixed tool names remain `run_js`, `session_open`, `session_eval`, and
`session_close`. Update saved tool references if you change the registration name.

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

[Unreleased]: https://github.com/zbrdc/mossbed/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/zbrdc/mossbed/releases/tag/v0.2.0
[0.1.0]: https://github.com/zbrdc/mossbed/releases/tag/v0.1.0

