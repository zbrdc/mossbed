// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Boundary tests: what a guest is prevented from doing, asserted against the
//! real binary.
//!
//! These document observed behaviour rather than attacking it. There is no
//! fuzzing and no attempt to exploit V8 here; the point is that each known route
//! out of a guest fails, and fails in a way a test can name. Every expectation
//! below was measured before it was written down, and the comment above it says
//! how, so a change in tardigrade shows up as a failing test rather than as
//! silently weaker documentation.
//!
//! They skip when the binary is absent, like the rest of the suite.

use tardigrade_runner::{RunOptions, RunStatus, Runner, resolve_bin};

fn runner() -> Option<Runner> {
    let bin = resolve_bin(None);
    if !bin.is_file() {
        eprintln!(
            "SKIP: tardigrade binary not found at {} (set TARDIGRADE_BIN to override)",
            bin.display()
        );
        return None;
    }
    let mut runner = Runner::new(bin).expect("runner config");
    runner.sandbox_check().expect("tardigrade --sandbox-check");
    Some(runner)
}

fn run(runner: &Runner, code: &str) -> String {
    runner
        .run_js(code, &RunOptions::default())
        .expect("run")
        .stdout
}

/// vm.net is the only channel-shaped API a guest has. Under this server's flags
/// it is inert: listen accepts a handler, nothing is ever delivered to it, and
/// send fails. Tardigrade's seccomp deny list refuses every socket call with
/// EPERM as well, so even a working API would have no route out.
#[test]
fn guest_cannot_reach_the_network() {
    let Some(runner) = runner() else { return };
    let stdout = run(
        &runner,
        r#"const r = { delivered: null };
try { vm.net.listen(1, function (m) { r.delivered = String(m); }); r.listen = "accepted"; } catch (e) { r.listen = "threw"; }
try { vm.net.send(1, "hello"); r.send = "accepted"; } catch (e) { r.send = String(e).slice(0, 40); }
setTimeout(() => console.log(JSON.stringify(r)), 1);"#,
    );
    assert!(
        stdout.contains(r#""delivered":null"#),
        "stdout was {stdout:?}"
    );
    assert!(
        stdout.contains(r#""send":"Error: no listener"#),
        "send must not be accepted, stdout was {stdout:?}"
    );
}

/// No module loader and no way to start a process: tardigrade denies execve,
/// fork and clone in seccomp, and neither the CommonJS nor the worker surface is
/// present to reach them from.
#[test]
fn guest_cannot_load_modules_or_start_processes() {
    let Some(runner) = runner() else { return };
    let stdout = run(
        &runner,
        r#"const absent = ["require", "process", "module", "Worker", "SharedWorker", "importScripts", "child_process", "spawn", "exec", "fork", "Deno", "Bun"];
console.log(JSON.stringify(absent.filter((n) => typeof globalThis[n] !== "undefined")));"#,
    );
    assert_eq!(stdout.trim(), "[]", "stdout was {stdout:?}");
}

/// The guest's filesystem is its own; the host's is not merely unreadable but
/// absent. Landlock would deny the reads anyway, and the guest's namespace does
/// not contain the paths in the first place.
#[test]
fn host_filesystem_paths_are_invisible() {
    let Some(runner) = runner() else { return };
    let stdout = run(
        &runner,
        r#"const seen = { visible: [], readable: [] };
for (const p of ["/etc/hostname", "/etc/passwd", "/proc/self/environ", "/dev/urandom", "/root/.ssh", "/tmp", "/home"]) {
  try { if (vm.fs.exists(p)) { seen.visible.push(p); } } catch (e) {}
  try { vm.fs.read(p); seen.readable.push(p); } catch (e) {}
}
console.log(JSON.stringify(seen));"#,
    );
    assert!(
        stdout.contains(r#""visible":[]"#) && stdout.contains(r#""readable":[]"#),
        "no host path may be visible or readable, stdout was {stdout:?}"
    );
}

/// A guest that allocates without bound stops at tardigrade's --memory default
/// rather than at the 2 GiB address-space belt, and it stops in a way it can
/// catch: 256 MiB of ArrayBuffers succeed, then a RangeError, with the process
/// still exiting 0. This runner passes no --memory, so that default is the
/// effective bound, several times lower than RLIMIT_AS.
#[test]
fn runaway_allocation_is_capped_and_recoverable() {
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js(
            r#"const held = []; let n = 0, err = null;
try { for (; n < 200; n++) { held.push(new Uint8Array(16 * 1024 * 1024).fill(7)); } }
catch (e) { err = String(e); }
console.log(JSON.stringify({ chunks: n, mib: n * 16, err: err }));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert_eq!(result.status, RunStatus::Ok, "the cap must be catchable");
    assert_eq!(result.exit_code, Some(0), "and must not kill the guest");
    assert!(
        result.stdout.contains("Array buffer allocation failed"),
        "stdout was {:?}",
        result.stdout
    );
    let mib: u32 = result
        .stdout
        .split(r#""mib":"#)
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|v| v.parse().ok())
        .expect("mib in output");
    assert!(
        (128..1024).contains(&mib),
        "expected a bound near tardigrade's 256 MiB default, got {mib} MiB"
    );
}

/// WebAssembly executes inside the guest, and wasm memory is what actually
/// reaches the address-space belt: a modest buffer is fine, a large one fails
/// cleanly with a RangeError rather than crashing the process.
#[test]
fn wasm_executes_and_is_bounded_by_the_address_space_cap() {
    let Some(runner) = runner() else { return };
    let stdout = run(
        &runner,
        r#"const out = {};
// hand-encoded module exporting f() { return 42 }
const mod42 = new Uint8Array([0,97,115,109,1,0,0,0,1,5,1,96,0,1,127,3,2,1,0,7,5,1,1,102,0,0,10,6,1,4,0,65,42,11]);
const inst = new WebAssembly.Instance(new WebAssembly.Module(mod42), {});
out.calls = inst.exports.f();
try { out.small = new WebAssembly.Memory({ initial: 2048 }).buffer.byteLength; } catch (e) { out.small = "threw"; }
try { out.large = String(new WebAssembly.Memory({ initial: 8192 }).buffer.byteLength); } catch (e) { out.large = String(e); }
console.log(JSON.stringify(out));"#,
    );
    assert!(stdout.contains(r#""calls":42"#), "stdout was {stdout:?}");
    assert!(
        stdout.contains(r#""small":134217728"#),
        "stdout was {stdout:?}"
    );
    assert!(
        stdout.contains("could not allocate memory"),
        "a 512 MiB wasm buffer must fail cleanly, stdout was {stdout:?}"
    );
}

/// Stack exhaustion is an ordinary JavaScript error here, not a crash: the guest
/// catches it and still exits 0.
#[test]
fn stack_exhaustion_is_a_catchable_error() {
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js(
            r#"let depth = 0, err = null;
function f(n) { depth = n; return n > 0 ? f(n + 1) : 0; }
try { f(1); } catch (e) { err = String(e); }
console.log(JSON.stringify({ depth: depth, err: err }));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert_eq!(result.status, RunStatus::Ok);
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result.stdout.contains("Maximum call stack size exceeded"),
        "stdout was {:?}",
        result.stdout
    );
}

/// The heap cap is an operator setting, so it never appears in the tool schema,
/// and it is reported back. Raising it must also raise the address-space belt:
/// the belt has to cover the V8 reservations plus the heap, or V8 aborts at
/// startup instead of the guest being capped.
#[test]
fn the_heap_cap_is_reported_and_operator_raisable() {
    let Some(runner) = runner() else { return };
    assert_eq!(runner.heap_mb(), 256, "the tardigrade default, stated");
    let capped = runner
        .run_js(
            "const held = []; let n = 0; try { for (; n < 200; n++) held.push(new Uint8Array(16*1024*1024)); } catch (e) {}
console.log(JSON.stringify({mib: n * 16}));",
            &RunOptions::default(),
        )
        .expect("run");
    assert_eq!(capped.heap_limit_mb, 256);
    assert!(
        capped.stdout.contains("\"mib\":256"),
        "stdout was {:?}",
        capped.stdout
    );

    let roomy = runner.with_heap_mb(1024);
    let bigger = roomy
        .run_js(
            "const held = []; let n = 0; try { for (; n < 200; n++) held.push(new Uint8Array(16*1024*1024)); } catch (e) {}
console.log(JSON.stringify({mib: n * 16}));",
            &RunOptions::default(),
        )
        .expect("run");
    assert_eq!(bigger.heap_limit_mb, 1024);
    assert!(
        bigger.status == RunStatus::Ok,
        "the guest must survive: {:?}",
        bigger.stderr
    );
    let mib: u32 = bigger
        .stdout
        .split("\"mib\":")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|v| v.parse().ok())
        .expect("mib in output");
    assert!(
        mib > 256,
        "a raised cap must actually allow more, got {} MiB",
        mib
    );
}

/// Tardigrade refuses --memory below 8 MiB, and so does this, before spawning.
#[test]
fn a_heap_cap_below_the_minimum_is_refused() {
    let Some(runner) = runner() else { return };
    let refused = runner
        .with_heap_mb(4)
        .run_js("1+1;", &RunOptions::default());
    assert!(refused.is_err(), "4 MiB must be refused");
    assert!(refused.unwrap_err().contains("at least 8"));
}
