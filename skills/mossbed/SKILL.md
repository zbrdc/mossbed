---
name: mossbed
description: Sandboxed JavaScript and WebAssembly execution with mossbed, including TypeScript and compiled C/C++/Rust workflows. Use when running generated code, transforming data, testing algorithms, or keeping isolate state across MCP calls.
---

# Mossbed

Use mossbed for generated code that needs no unrestricted host access.
Keep normal project builds and tests in their native toolchains.

## Discover and check the connection

Read the live MCP tool schemas before calling them. Client prefixes depend on registration names.
With a `mossbed` registration, Pi exposes `mossbed_run_js`, `mossbed_session_open`, `mossbed_session_eval`, and `mossbed_session_close`.

Call `run_js` with `{"code":"console.log(6 * 7)"}`.
Require `status: "ok"`, `exit_code: 0`, and `stdout` containing `42`.
On a missing binary or sandbox refusal, stop and report the error.
Do not bypass the sandbox or silently substitute host execution.
See the [installation instructions](https://github.com/zbrdc/mossbed#prerequisites).

## Choose the language path

| Source | Preparation |
|---|---|
| JavaScript | Send JavaScript directly. |
| TypeScript | Transpile to JavaScript. Bundle dependencies for the guest environment. |
| C, C++, Rust | Compile to WebAssembly. Load the module from JavaScript and supply required imports. |
| Other languages | Supply a compatible JavaScript or WebAssembly build, plus any required runtime. |

Send JavaScript in `code` and `entry_code`, not raw TypeScript or C/C++/Rust source.
Do not send native executables. Mossbed bundles no compiler toolchains or WASI runtime.
Host compilation and package scripts run outside the isolate. Treat untrusted builds as a separate security boundary.
An explicitly supplied WebAssembly compiler can run inside the guest, with its required imports.

For binary modules, use version-2 snapshot files with `b64` entries in the `preload` JSON string.
Load their bytes with `vm.fs.readBytes`, then use `WebAssembly.Module` and `WebAssembly.Instance`.
When the module requires WASI imports, supply a compatible WASI shim.
See [language examples](https://github.com/zbrdc/mossbed/blob/master/docs/LANGUAGES.md).

## Choose the lifetime

- Use `run_js` for one-shot execution. Each call starts a fresh engine process.
- Use `session_open` for persistent globals, WebAssembly instances, timers, and RAM files.
  Initialize state with JavaScript in `entry_code`.
- Pass the returned `session_id` to `session_eval`. Send one JavaScript expression on one line.
  Wrap statements in an expression, such as `(() => { globalThis.n += 1; return n; })()`.
- When finished, always call `session_close`, including after errors. Do not leave test sessions open.

Sessions default to a 300000 ms lifetime. Set `ttl_ms` up to 3600000 ms, not an indefinite lifetime.
Each evaluation defaults to a 10000 ms timeout, set through `session_open.timeout_ms`.
`MOSSBED_SESSION_IDLE_MS` defaults to 300000. `MOSSBED_MAX_SESSIONS` defaults to 8.
Session tools require an engine with `--serve` support.
For long idle sessions in Pi, have the operator configure the MCP adapter's `lazy-keep-alive` lifecycle.

## Snapshots and time

Use `dump: true` to capture RAM files. Pass the returned `dump` string as the next call's `preload`.
Snapshots do not preserve globals, timers, or instantiated WebAssembly modules. Use sessions for those.
Restoration merges files and overwrites matching paths. It does not clear unrelated files.
Keep snapshots within `MOSSBED_INPUT_MAX` and `MOSSBED_DUMP_MAX`.

Time is virtual by default: `Date.now()` starts at 0.
Use `clock: "real"` for wall-clock time or `epoch_ms` with virtual time for a fixed base.
Use `seed` to pin `Math.random`. Do not assume network responses or measured durations are deterministic.

## Guest APIs and limits

- Use `vm.fs` for RAM files, not host paths. The guest is neither Node.js nor a browser.
  Do not assume `require`, `process`, `Buffer`, or `fetch` exists.
- Use `module: true` for ES modules with a compatible engine. Imports resolve inside the RAM filesystem.
  Bundle bare package imports first.
- Use `JSON.stringify` for structured console output. Objects otherwise print as `[object Object]`.
  Console methods are `log`, `info`, `warn`, and `error`. Session console output goes to stderr.
- Encode Unicode before `btoa`. It accepts Latin-1, not arbitrary Unicode.
- Avoid `Intl` and locale-formatting APIs. Current tardigrade stubs them with catchable errors, not process crashes.
- Use `vm.net.request` only for operator-granted capability names. The guest is offline without grants.
  Do not assume arbitrary URLs, host sockets, or network access are available.

## Safety

Mossbed requires Landlock and seccomp, and applies deadlines, memory settings, and RAM-filesystem quotas.
Each run or session has its own engine process. An engine abort destroys that guest's state.
Do not run deliberate allocation bombs, huge fill/split operations, or crash probes on shared deployments.
Do not assume every allocation failure is catchable.

Operator ceilings use `MOSSBED_*` names, including `MOSSBED_MEM`, `MOSSBED_AS`, and `MOSSBED_FS_QUOTA`.
`TARDIGRADE_BIN` selects the engine. Do not raise ceilings or set `TARDIGRADE_ALLOW_UNCAPPED` to evade refusals.
`memory_cap_mb` reports the virtual address-space limit, not resident memory or the heap budget.
`MOSSBED_MEM` is not a total-process memory cap. Module-defined WebAssembly memory can bypass JavaScript allocation accounting.
Use external memory limits for hostile workloads. See [Security](https://github.com/zbrdc/mossbed/blob/master/SECURITY.md).

Treat the live schemas and the engine's [guest API](https://github.com/zbrdc/tardigrade/blob/main/docs/GUEST-API.md) as authoritative.
