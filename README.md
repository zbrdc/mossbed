# <img src="assets/logo.svg" alt="" width="72" align="middle"> tardigrade-runner

Minimal MCP stdio server that lets agents (pi coding agent) run generated
JavaScript safely. It wraps [tardigrade](https://github.com/zbrdc/tardigrade), the sandboxed Rust V8
runtime, and exposes exactly one tool, `run_js`: the code goes to a temp file
and runs as `tardigrade --sandbox [--clock=virtual] --seed=N --timeout=MS <file>` inside a
Landlock + seccomp caged process with a RAM-only filesystem and no host APIs.

## Limits and fail-closed behavior

- **Fail closed:** at startup the server runs `tardigrade --sandbox-check` and
  requires that check to exit successfully. On any other outcome the server
  stays up but refuses every `run_js` call with a clear error.
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
- **Script handling:** guest code is written to
  `$TMPDIR/tardigrade-runner-<pid>/<n>.js` — a private `0700` directory, a `0600`
  file created with `O_EXCL` so a pre-planted symlink cannot redirect the write
  — and removed when the run ends. The directory itself stays, for reuse by
  later runs.

## Build

    cargo build --release

Requires Rust 1.85 or newer (the crate is edition 2024) and the tardigrade
binary, either as a sibling checkout at `../tardigrade/target/release/tardigrade`
or wherever `TARDIGRADE_BIN` points. Linux in practice: the cage is Landlock +
seccomp, the memory cap is `prlimit`, and the gate above refuses to run anything
when the cage is unavailable.

## Pi hookup

    pi mcp add tardigrade -- /absolute/path/to/tardigrade-runner/target/release/tardigrade-runner

Binary resolution: `TARDIGRADE_BIN` env var, else `--tardigrade=<path>` flag,
else `../tardigrade/target/release/tardigrade` relative to the working
directory.

## The `run_js` tool

Speaks MCP over stdio, one JSON-RPC object per line, protocol version
`2024-11-05`.

| argument | required | default | notes |
|---|---|---|---|
| `code` | yes | — | JavaScript source |
| `timeout_ms` | no | `10000` | `1..=3600000`; one hour is the ceiling |
| `seed` | no | `0` | passed to tardigrade, for repeatable runs |
| `clock` | no | `virtual` | `virtual` or `real`; virtual is what makes runs reproducible |

The result comes back as JSON text: `status` (`ok`, `timeout` or `error`),
`exit_code` (`null` when a signal killed the run), `stdout` and `stderr` (each
capped at 64 KiB and marked `…[truncated]`), `duration_ms`, and `memory_cap_mb`
(`null` only for a run explicitly allowed to go uncapped).

A refused or failed run is reported as tool-error content rather than a protocol
error, so the caller can read why.

## Tests

    cargo test

Unit tests pin the refusal table, the output cap and its marker, truncation on a
character boundary, the private directory and file modes, write-error
propagation through the request loop, and the protocol version the server
answers with. The four integration tests exercise the real tardigrade binary,
and skip themselves with a message when it is not on disk.

## Licence

Apache-2.0, see [LICENSE](LICENSE).
