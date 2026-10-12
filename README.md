<img src="assets/logo.svg" alt="tardigrade-runner" width="220">

# tardigrade-runner

Minimal MCP stdio server that lets agents (pi coding agent) run generated
JavaScript safely. It wraps [tardigrade](https://github.com/zbrdc/tardigrade), the sandboxed Rust V8
runtime, and exposes exactly one tool, `run_js`: the code goes to a temp file
and runs as `tardigrade --sandbox --clock --seed --timeout <file>` inside a
Landlock + seccomp caged process with a RAM-only filesystem and no host APIs.

## Limits and fail-closed behavior

- **Fail closed:** at startup the server runs `tardigrade --sandbox-check`.
  Unless it reports `supported`, the server stays up but refuses every
  `run_js` call with a clear error.
- **Hard deadline:** the caller's `timeout_ms` (default 10 000) is passed to
  tardigrade's own `--timeout` watchdog *and* enforced by this parent, which
  SIGKILLs the child after a 500 ms grace — so the guest's clean exit 124
  wins when it can, and a stuck child still dies on time.
- **Memory cap:** runs are wrapped in `prlimit --as=<MiB>` (default
  `TARDIGRADE_RUNNER_AS=2048`; note V8 aborts at startup below ~1.3 GiB of
  address space, so smaller caps break every run). If `prlimit` is not on
  PATH, `run_js` fails closed with an error; set `TARDIGRADE_ALLOW_UNCAPPED=1`
  to proceed uncapped, reported honestly as `memory_cap_mb: null`.
- Result stdout/stderr are captured and truncated to 64 KiB each.

## Pi hookup

    pi mcp add tardigrade -- /home/dan/git/tardigrade-runner/target/release/tardigrade-runner

Binary resolution: `TARDIGRADE_BIN` env var, else `--tardigrade=<path>` flag,
else `../tardigrade/target/release/tardigrade` relative to the working
directory.
