// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Integration tests against the real tardigrade binary (default
//! ../tardigrade/target/release/tardigrade, or TARDIGRADE_BIN).
//! Each test skips with a message when the binary is not found.

use tardigrade_runner::{DRAIN_CAP_BYTES, RunOptions, RunStatus, TRUNCATED_MARKER};

mod common;
use common::runner;

#[test]
fn hello_run_returns_ok_and_captures_stdout() {
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js(
            r#"console.log("hello from runner");"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert_eq!(result.status, RunStatus::Ok);
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result.stdout.contains("hello from runner"),
        "stdout was: {:?}",
        result.stdout
    );
}

#[test]
fn vm_exit_minus_one_propagates_as_exit_255() {
    // Asserts the tardigrade exit-code fix: vm.exit(-1) must surface as 255.
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js("vm.exit(-1);", &RunOptions::default())
        .expect("run");
    assert_eq!(result.exit_code, Some(255));
}

#[test]
fn infinite_loop_times_out_quickly() {
    let Some(runner) = runner() else { return };
    let opts = RunOptions {
        timeout_ms: 300,
        ..RunOptions::default()
    };
    let result = runner.run_js("for(;;){}", &opts).expect("run");
    assert_eq!(result.status, RunStatus::Timeout);
    assert!(
        result.duration_ms < 2000,
        "duration was {} ms",
        result.duration_ms
    );
}

#[test]
fn chatty_guest_finishes_with_capped_output() {
    // ~1 MiB of output (1024 lines x 1025 bytes), far past one pipe buffer.
    let Some(runner) = runner() else { return };
    let code = "const s = \"x\".repeat(1024);\nfor (let i = 0; i < 1024; i++) console.log(s);";
    let opts = RunOptions {
        timeout_ms: 10_000,
        ..RunOptions::default()
    };
    let result = runner.run_js(code, &opts).expect("run");
    assert_eq!(
        result.status,
        RunStatus::Ok,
        "duration {} ms",
        result.duration_ms
    );
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(
        result.stdout.len(),
        DRAIN_CAP_BYTES + TRUNCATED_MARKER.len()
    );
    assert!(result.stdout.ends_with(TRUNCATED_MARKER));
    assert!(
        result.duration_ms < 5_000,
        "duration was {} ms",
        result.duration_ms
    );
}

#[test]
fn same_seed_gives_identical_output_including_the_virtual_clock() {
    // The documented default is a virtual clock and seed 0, so two runs must
    // agree byte for byte, and virtual time starts at the epoch.
    let Some(runner) = runner() else { return };
    let code = "console.log(Date.now(), Math.random());";
    let first = runner.run_js(code, &RunOptions::default()).expect("run");
    let second = runner.run_js(code, &RunOptions::default()).expect("run");
    assert_eq!(first.stdout, second.stdout, "seeded runs must not diverge");
    assert!(
        first.stdout.starts_with("0 "),
        "virtual clock should start at the epoch, got {:?}",
        first.stdout
    );
}

#[test]
fn timer_chain_still_hits_the_deadline() {
    // A guest that keeps rescheduling timers never lets its event loop drain.
    // The deadline must still hold, because this parent enforces it and a timer
    // cannot buy the guest more wall-clock time.
    let Some(runner) = runner() else { return };
    let opts = RunOptions {
        timeout_ms: 300,
        ..RunOptions::default()
    };
    let result = runner
        .run_js("(function again() { setTimeout(again, 1000); })();", &opts)
        .expect("run");
    assert_eq!(result.status, RunStatus::Timeout);
    assert!(
        result.duration_ms < 2000,
        "duration was {} ms",
        result.duration_ms
    );
}

#[test]
fn guest_filesystem_is_ephemeral_and_never_the_hosts() {
    // The README claims vm.fs is a RAM filesystem: writable in a run, gone
    // afterwards, and unable to see the host's filesystem at all.
    let Some(runner) = runner() else { return };
    let code = r#"let wrote = false, readBack = null, hostVisible = true;
try { vm.fs.write("/probe.txt", "hello"); wrote = true; readBack = String(vm.fs.read("/probe.txt")); } catch (e) {}
try { hostVisible = vm.fs.exists("/etc/hostname"); } catch (e) { hostVisible = "threw"; }
console.log(JSON.stringify({ wrote: wrote, readBack: readBack, hostVisible: hostVisible }));"#;
    let first = runner.run_js(code, &RunOptions::default()).expect("run");
    assert!(
        first.stdout.contains(r#""wrote":true"#)
            && first.stdout.contains(r#""readBack":"hello""#)
            && first.stdout.contains(r#""hostVisible":false"#),
        "stdout was {:?}",
        first.stdout
    );

    let second = runner
        .run_js(
            r#"console.log(JSON.stringify({ stillThere: vm.fs.exists("/probe.txt") }));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert!(
        second.stdout.contains(r#""stillThere":false"#),
        "a file must not survive into the next run, stdout was {:?}",
        second.stdout
    );
}

#[test]
fn guest_has_no_node_or_browser_escape_hatches() {
    let Some(runner) = runner() else { return };
    let code = r#"const banned = ["require", "process", "fetch", "Deno", "Bun", "XMLHttpRequest", "WebSocket", "Buffer"];
console.log(JSON.stringify(banned.filter((n) => typeof globalThis[n] !== "undefined")));"#;
    let result = runner.run_js(code, &RunOptions::default()).expect("run");
    assert_eq!(
        result.stdout.trim(),
        "[]",
        "unexpected host globals reachable: {:?}",
        result.stdout
    );
}
