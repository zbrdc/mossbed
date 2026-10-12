# <img src="assets/logo.svg" alt="" width="72" align="middle"> tardigrade-runner

Minimal MCP stdio server that lets agents (pi coding agent) run generated
JavaScript safely. It wraps [tardigrade](https://github.com/zbrdc/tardigrade),
the sandboxed Rust V8 runtime, and exposes exactly one tool, `run_js`. It fails
closed: where the platform cannot provide tardigrade's Landlock + seccomp cage,
the server stays up and refuses every run rather than executing anything
unconfined.

## Build

    cargo build --release

Requires Rust 1.85 or newer, a Linux kernel that tardigrade's cage supports, and
the tardigrade binary itself.

## Run

    pi mcp add tardigrade -- /absolute/path/to/tardigrade-runner/target/release/tardigrade-runner

Binary resolution: `TARDIGRADE_BIN` env var, else `--tardigrade=<path>` flag, else
`../tardigrade/target/release/tardigrade` relative to the working directory.

The tool's arguments and result schema are documented where a client reads them:
the `run_js` tool description in `src/main.rs`. The runner's own decisions -
the flags it passes, the deadline, the caps, the note it adds to a guest's
stderr - are documented at the code that makes them in `src/lib.rs`. The guest
itself is tardigrade's to document: see its `GUEST-API.md`, `OPTIONS.md`,
`SANDBOX.md` and `KNOWN_BUGS.md`.

## Licence

Apache-2.0, see [LICENSE](LICENSE).
