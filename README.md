# <img src="assets/logo.svg" alt="" width="72" align="middle"> tardigrade-runner

Minimal MCP stdio server that lets agents (pi coding agent) run generated
JavaScript safely. It wraps [tardigrade](https://github.com/zbrdc/tardigrade), the sandboxed Rust V8
runtime, and exposes exactly one tool, `run_js`: the code goes to a temp file
and runs as `tardigrade --sandbox [--clock=virtual] --seed=N --timeout=MS <file>` inside a
Landlock + seccomp caged process. What the guest can reach is small, and is
described under [The guest environment](#the-guest-environment).

## The guest environment

The guest's globals are plain ECMAScript plus a small deliberate surface:
`console`, `setTimeout`/`setInterval`/`clearTimeout`/`clearInterval`,
`TextEncoder`/`TextDecoder`, `atob`/`btoa`, `structuredClone`,
`performance`, `WebAssembly`, and tardigrade's own `vm` object. All of the
following was measured against the real binary, not read off its source:

- No `require`, `process`, `fetch`, `Deno`, `Bun`, `XMLHttpRequest`,
  `WebSocket` or `Buffer`.
- `vm.fs` is a working filesystem API — `read`, `readBytes`, `write`,
  `list`, `remove`, `mkdir`, `exists`, `stat`, `readdir`, `rename` — but
  it reaches the guest's own RAM filesystem and nothing else. A file written at
  `/probe.txt` reads back during that run and `list("/")` shows it; the next
  run starts with an empty root. The host's filesystem is invisible:
  `vm.fs.exists("/etc/hostname")` is `false`. Paths must be absolute.
- `vm` also carries `net` (`port`, `listen`, `send`, `close`), `stdin`,
  `args`, `exit`, `snapshot`, `restore`, `run` and `version`. This server
  passes the guest no arguments and no stdin. `vm.net` is unused by this server
  and is not covered by its tests.

**What works, and what silently misleads.** Measured against the real binary,
because these are the things an agent gets wrong rather than the things it gets right:

- **Time is virtual by default.** `Date.now()` is `0` and `new Date().toISOString()`
  is `1970-01-01T00:00:00.000Z` unless the caller passes `clock: "real"`, which
  gives wall-clock time. Nothing warns you; date arithmetic just comes out 1970.
- **Do not call the Intl date APIs.** ICU data is not loaded: `Intl.NumberFormat`
  throws `Internal error. Icu error.`, and `Intl.DateTimeFormat`,
  `Date.prototype.toLocale*String` and `resolvedOptions()` abort the whole guest
  process with a fatal V8 error (exit 133). This runner reports that as an error and
  keeps serving, but the guest is gone. It is a known tardigrade bug, listed in its
  own KNOWN_BUGS.md.
- **No crypto.** There is no `crypto` and no `SubtleCrypto`. `atob` and `btoa` exist,
  so encoding works and hashing does not.
- **Recursion stops near 10 500 frames**, and the JS heap and ArrayBuffer are each
  capped at 256 MiB. Both arrive as catchable `RangeError`s, several times below the
  2 GiB address-space belt.
- **Plenty is fast enough for real work**: a 12 MB JSON document parses in ~200 ms, a
  100 MB string can be built, a million floats sort in ~50 ms, a 5 MB regex scan takes
  ~90 ms, and `BigInt`, `TextEncoder`, `structuredClone` and WebAssembly all work.

## Limits and fail-closed behavior

- **Fail closed:** at startup the server runs `tardigrade --sandbox-check` and
  requires that check to exit successfully. On any other outcome the server
  stays up but refuses every `run_js` call with a clear error.
- **Hard deadline:** the caller's `timeout_ms` (default 10 000) is passed to
  tardigrade's own `--timeout` watchdog *and* enforced by this parent, which
  SIGKILLs the child after a 500 ms grace — so the guest's clean exit 124
  wins when it can, and a stuck child still dies on time.
- **Virtual time:** with the default `clock: "virtual"`, time is frozen while
  the script runs synchronously and then jumps to the next scheduled timer, so
  `setTimeout(f, 3_600_000)` completes in milliseconds of wall time and the
  clock still advances by an hour. `clock: "real"` uses wall-clock time
  instead. Timers cannot buy a run more time: a self-rescheduling timer chain
  still ends at `timeout_ms` with exit 124.
- **Memory cap:** runs are wrapped in `prlimit --as=<MiB>` (default
  `TARDIGRADE_RUNNER_AS=2048`; note V8 aborts at startup below ~1.3 GiB of
  address space, so smaller caps break every run). If `prlimit` is not on
  PATH, `run_js` fails closed with an error; set `TARDIGRADE_ALLOW_UNCAPPED=1`
  to proceed uncapped, reported honestly as `memory_cap_mb: null`.
- Result stdout/stderr are captured and truncated to 64 KiB each.
- **Script handling:** guest code is written to
  `$TMPDIR/tardigrade-runner-<pid>/<n>.js` — a private `0700` directory, a `0600`
  file created with `O_EXCL` so a pre-planted symlink cannot redirect the write
  — and removed when the run ends. The directory holding it is reused by later
  runs in the same process, and removed when the server exits.

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

What an agent receives is smaller than what this server sends. Streams are
capped at 64 KiB here, but a pi MCP client replaces any reply whose first line
exceeds 50 KiB with a short prose notice and writes the full text to a file under
/tmp/pi-mcp-output-*/, so the practical ceiling is about 50 KiB per call. A
client that expects the JSON shape has to handle that notice, and the notice is
not JSON. Nothing survives between calls either: no globals, no files, no
arguments and no stdin, so every call must carry its own input.

## Tests

    cargo test

Unit tests pin the refusal table, the output cap and its marker, truncation on a
character boundary, the private directory and file modes, write-error
propagation through the request loop, and the protocol version the server
answers with. The integration tests exercise the real tardigrade binary. When it
is absent they print a SKIP line and pass without running anything, so a green
suite does not by itself prove they ran; set TARDIGRADE_BIN to point at one.

A separate boundary suite, tests/sandbox.rs, asserts what a guest is
prevented from doing: no network (vm.net is inert, seccomp denies socket calls
with EPERM), no module loading or process creation, host filesystem paths
invisible rather than merely unreadable, runaway allocation stopped at
tardigrade's 256 MiB default as a catchable RangeError, WebAssembly executing but
bounded by the address-space belt, and stack exhaustion caught rather than
crashing. These document observed behaviour; they are not an attempt to break the
sandbox, and no fuzzing is involved.

## Throughput

`examples/load` drives the runner directly from N threads, without the MCP line
protocol in the path:

    cargo run --release --example load -- 32 3000

On a 24-core machine, 3000 runs per level, `1+1;` per run:

| threads | runs/s | p50 | p99 |
|---|---|---|---|
| 1 | 185 | 5 ms | 10 ms |
| 8 | 1095 | 5 ms | 10 ms |
| 32 | 2546 | 11 ms | 19 ms |
| 64 | 2572 | 23 ms | 49 ms |
| 256 | 2515 | 76 ms | 259 ms |

Throughput saturates at about 32 concurrent runs and stays flat to 256, where
the machine is running 21 of its 24 cores busy and every extra worker only adds
queueing: p50 goes from 11 ms to 76 ms for no gain. Each run costs roughly 8 ms
of CPU, so the ceiling is cores divided by that. A guest that computes rather
than merely starting up is slower to serve: the same sweep with a
1e6-iteration loop peaks at 1028 runs/s.

These numbers price process start plus V8 init, because every run is a fresh
process and there is no warm path to amortise it over. Long-lived guests cost
more memory too: roughly 5 MiB transient for a run lasting a few milliseconds,
around 28 MiB each once a guest lives for a few hundred.

## Licence

Apache-2.0, see [LICENSE](LICENSE).
