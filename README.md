<p align="center">
  <img src="assets/icon-readme.png" width="256" alt="Mossbed square moss-tile logo">
</p>
<h1 align="center">mossbed</h1>

Minimal MCP stdio server that lets AI agents run generated
JavaScript safely. It wraps [tardigrade](https://github.com/zbrdc/tardigrade),
the sandboxed Rust V8 runtime. Its tools are `run_js` for stateless one-shot
runs and, where the tardigrade binary offers `--serve`,
`session_open`/`session_eval`/`session_close` for persistent sessions whose
guest state survives between evaluations. It fails closed: where the platform
cannot provide tardigrade's Landlock + seccomp cage, or the binary is too old
for the flags this runner passes, the server stays up and refuses every run
rather than executing anything unconfined.

Requests are handled concurrently and `run_js` calls can be cancelled with
the standard MCP `notifications/cancelled`. Tool results are returned as
text and as `structuredContent`.

## Prerequisites

Mossbed requires a separately installed [tardigrade](https://github.com/zbrdc/tardigrade)
runtime. Building mossbed does not install it.

- **Linux x86_64**, with Landlock ABI 6 or newer (Linux 6.12+) and seccomp
  with TSYNC enabled. Kernel version alone is not enough: the sandbox check
  below must succeed in the same host or container where your MCP client runs.
- **`prlimit`**, from util-linux, available on the MCP server's `PATH`.
- **Rust and Cargo** for source builds. Mossbed requires Rust 1.85 or newer;
  tardigrade's CI currently tests Rust 1.98.0, used in the commands below.
- **A native C/C++ compiler and linker** for the Rust dependencies.
- **Git and network access** for the initial build, including the prebuilt
  V8 library downloaded from rusty_v8's GitHub releases.

See tardigrade's [installation guide](https://github.com/zbrdc/tardigrade/blob/main/docs/INSTALL.md)
and [sandbox requirements](https://github.com/zbrdc/tardigrade/blob/main/docs/SANDBOX.md)
for current upstream requirements. Mossbed cannot execute guests on macOS,
Windows, or Linux hosts without the required sandbox features.

### Install tardigrade

There are currently no upstream release binaries. From a directory where you
keep source checkouts, build the latest default-branch source and install the
runtime at a stable path:

```sh
git clone --depth 1 https://github.com/zbrdc/tardigrade.git
rustup toolchain install 1.98.0 --profile minimal
cargo +1.98.0 build --release --locked --bin tardigrade \
  --manifest-path tardigrade/Cargo.toml --target-dir tardigrade/target
install -Dm755 tardigrade/target/release/tardigrade "$HOME/.local/bin/tardigrade"
"$HOME/.local/bin/tardigrade" --sandbox-check
command -v prlimit
```

The sandbox check must exit successfully. If it fails, resolve the reported
host or container restriction before using mossbed; it will not run guests
without the sandbox. The HTTP bridge is not needed.

Mossbed also checks engine capabilities: `--fs-quota` is required,
`--serve` enables session tools, and `--allow-call` is required when network
grants are configured. An older executable may be incompatible even if its
sandbox check passes.

## Build mossbed

    cargo build --release

## Run

Register mossbed as a stdio server in your MCP client. The client launches
the executable and communicates over stdin/stdout; no HTTP service is needed.

For clients that use an `mcpServers` configuration:

```json
{
  "mcpServers": {
    "mossbed": {
      "command": "/absolute/path/to/mossbed/target/release/mossbed",
      "env": {
        "TARDIGRADE_BIN": "/home/you/.local/bin/tardigrade"
      }
    }
  }
}
```

Replace both paths with your executable paths. Other clients use different
configuration formats; select stdio transport and set the same command and
environment variable.

Reconnect the MCP server after installing or replacing tardigrade, or changing
its path. To verify execution, call `run_js` with `{"code":"console.log(42)"}`;
the result should have `status: "ok"` and `stdout` containing `42`.

Binary resolution: `TARDIGRADE_BIN` env var, else `--tardigrade=<path>` flag, else
`../tardigrade/target/release/tardigrade` relative to the working directory when
it exists, else `tardigrade` on `PATH`. Whatever is found must still pass
`--sandbox-check` and a capability probe of its advertised flags.

Clients can add the registration name as a tool prefix, such as `mossbed_run_js`.

## Operator settings

Ceilings are operator settings, never tool arguments — a model must not be
able to lift its own limits or grant itself network access. Settings in this
table are environment variables read at startup:

| Variable | Default | Meaning |
|---|---|---|
| `MOSSBED_MEM` | `256` | guest heap/ArrayBuffer cap, MiB |
| `MOSSBED_AS` | `2048` | address-space belt floor (`prlimit --as`), MiB |
| `MOSSBED_FS_QUOTA` | `64` | RAM-filesystem quota, MiB (`0` disables) |
| `MOSSBED_INPUT_MAX` | `4194304` | largest `code`/`preload`/eval accepted, bytes |
| `MOSSBED_DUMP_MAX` | `1048576` | largest dump returned inline, bytes |
| `MOSSBED_ALLOW_CALL` | — | granted endpoints: `NAME=URL[,METHOD]` entries separated by `;` |
| `MOSSBED_NET_MAX_RESPONSE` | `1048576` | per-response byte cap for granted calls |
| `MOSSBED_NET_TOTAL_BYTES` | `8388608` | per-run network byte budget |
| `MOSSBED_NET_TIMEOUT_MS` | `30000` | per-request network timeout |
| `MOSSBED_NET_ALLOW_PRIVATE` | — | `1` lets granted endpoints resolve to private/loopback addresses |
| `MOSSBED_MAX_SESSIONS` | `8` | concurrent sessions |
| `MOSSBED_SESSION_IDLE_MS` | `300000` | idle time before a session is reaped |

`MOSSBED_AS` is a floor, not the belt that applies: the runner
raises it to at least 128 GiB plus the heap cap to cover V8's startup
address-space reservations. This is virtual address space, not resident RAM;
a smaller belt can abort V8 before guest code runs. `MOSSBED_MEM`,
not the belt, bounds the guest heap.

The existing `TARDIGRADE_ALLOW_UNCAPPED` escape hatch keeps its name.
If `prlimit` is absent, setting it to `1` permits runs without the parent
address-space limit. Mossbed checks this variable for each run or session.
Keep it unset unless you accept that reduced protection.

With no `MOSSBED_ALLOW_CALL`, guests are fully offline and
`vm.net.request` does not exist. With grants, the guest calls endpoints by
name only (`vm.net.request(name, body)`); URLs are never shown to the guest
or returned in results, and tardigrade's sidecar enforces the budgets and
address rules — see its `docs/NETWORK_CAPABILITIES.md`.

The tools' arguments and result schemas are documented where a client reads
them: the tool descriptions in `src/main.rs`. The runner's own decisions —
the flags it passes, the deadlines, the caps, the notes it adds to a guest's
stderr — are documented at the code that makes them in `src/lib.rs`. The
guest itself is tardigrade's to document: see its `docs/GUEST-API.md`,
`docs/OPTIONS.md`, `docs/SANDBOX.md` and `docs/KNOWN_BUGS.md`.

## Licence

Apache-2.0, see [LICENSE](LICENSE).
