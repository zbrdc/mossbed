# mossbed

<img src="assets/brand.png" width="720" alt="Mossbed: moss tile icon on the left and sheltered tardigrade mascot on the right">

Minimal MCP stdio server that lets agents (pi coding agent) run generated
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

## Build

    cargo build --release

Requires Rust 1.85 or newer, a Linux kernel that tardigrade's cage supports, and
the tardigrade binary itself.

## Run

    pi mcp add mossbed -- /absolute/path/to/mossbed/target/release/mossbed

Binary resolution: `TARDIGRADE_BIN` env var, else `--tardigrade=<path>` flag, else
`../tardigrade/target/release/tardigrade` relative to the working directory when
it exists, else `tardigrade` on `PATH`. Whatever is found must still pass
`--sandbox-check` and a capability probe of its advertised flags.

### Upgrade from tardigrade-runner

Mossbed is the new name for `tardigrade-runner`. The separate `tardigrade`
runtime keeps its name.

The GitHub repository is now
[`zbrdc/mossbed`](https://github.com/zbrdc/mossbed).

For an existing clone, update its remote:

    git remote set-url origin https://github.com/zbrdc/mossbed.git

- Update the MCP command path to the `mossbed` binary in your actual checkout.
- Replace each `TARDIGRADE_RUNNER_*` setting with its `MOSSBED_*` equivalent.
  The old names are no longer read. Migrate custom limits and grants before
  starting the new binary.
- Keep `TARDIGRADE_BIN` and `--tardigrade=<path>` unchanged.
- Use `mossbed` as the MCP registration name for new installations.
  Existing installations can retain the `tardigrade` registration name to
  preserve client-generated tool prefixes.
- Reconnect the MCP server after updating its configuration.

The server still exposes `run_js`, `session_open`, `session_eval`, and
`session_close`. Clients can add the registration name as a prefix, such as
`mossbed_run_js`. Update saved tool references if you change that name.

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

## Project marks

The moss tile is the compact application icon and favicon. The sheltered
tardigrade is the README mascot. Both use moss green on transparent backgrounds.
Use them on light surfaces. See [Mossbed marks](assets/README.md) for files,
dimensions, and minimum sizes.

## Licence

Apache-2.0, see [LICENSE](LICENSE).
