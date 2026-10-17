// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Integration tests against the real tardigrade binary (default
//! ../tardigrade/target/release/tardigrade, or TARDIGRADE_BIN).
//! Each test skips with a message when the binary is not found.

use mossbed::{DRAIN_CAP_BYTES, RunOptions, RunStatus, TRUNCATED_MARKER};

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

#[test]
fn dump_then_preload_hands_state_between_runs() {
    // The agent-loop pattern: run 1 writes state and dumps it; run 2 starts
    // from an empty RAM fs, preloads that snapshot, and continues the workflow.
    let Some(runner) = runner() else { return };
    let opts = RunOptions {
        dump: true,
        ..RunOptions::default()
    };
    let first = runner
        .run_js(
            "vm.fs.write(\"/state.json\", JSON.stringify({ visited: 1 }));",
            &opts,
        )
        .expect("run");
    assert_eq!(first.status, RunStatus::Ok);
    let snapshot = first.dump.expect("dump requested and run succeeded");
    assert!(snapshot.contains("visited"), "snapshot was: {snapshot}");

    let second = runner
        .run_js(
            r#"const s = JSON.parse(vm.fs.read("/state.json")); s.visited += 1; vm.fs.write("/state.json", JSON.stringify(s)); console.log("visited", s.visited);"#,
            &RunOptions {
                preload: Some(snapshot),
                ..RunOptions::default()
            },
        )
        .expect("run");
    assert_eq!(second.status, RunStatus::Ok, "stderr: {:?}", second.stderr);
    assert!(
        second.stdout.contains("visited 2"),
        "stdout: {:?}",
        second.stdout
    );
    assert_eq!(second.dump, None);
}

#[test]
fn invalid_preload_json_is_rejected_before_any_run() {
    let Some(runner) = runner() else { return };
    let opts = RunOptions {
        preload: Some("not json".into()),
        ..RunOptions::default()
    };
    let err = runner.run_js("1;", &opts).expect_err("must refuse");
    assert!(
        err.contains("preload must be a snapshot JSON object"),
        "{err}"
    );
}

#[test]
fn oversized_dump_is_suppressed_with_a_stderr_note() {
    let Some(runner) = runner() else { return };
    let runner = runner.with_dump_max(64);
    let code = "vm.fs.write(\"/big.txt\", \"x\".repeat(2048));";
    let opts = RunOptions {
        dump: true,
        ..RunOptions::default()
    };
    let result = runner.run_js(code, &opts).expect("run");
    assert_eq!(result.status, RunStatus::Ok);
    assert_eq!(result.dump, None);
    assert!(
        result.stderr.contains("dump suppressed"),
        "stderr: {:?}",
        result.stderr
    );
}

/// Caller arguments reach the guest as vm.args, and a u64-range seed
/// (past the old u32 ceiling) is accepted and stays deterministic.
#[test]
fn args_and_wide_seeds_reach_the_guest() {
    let Some(runner) = runner() else { return };
    let opts = RunOptions {
        seed: 5_000_000_001,
        args: vec!["alpha".into(), "beta gamma".into()],
        ..RunOptions::default()
    };
    let code = "console.log(JSON.stringify({args: vm.args, r: Math.random()}));";
    let first = runner.run_js(code, &opts).expect("run");
    let second = runner.run_js(code, &opts).expect("run");
    assert_eq!(first.status, RunStatus::Ok, "stderr: {:?}", first.stderr);
    assert!(
        first.stdout.contains(r#""args":["alpha","beta gamma"]"#),
        "stdout was {:?}",
        first.stdout
    );
    assert_eq!(first.stdout, second.stdout, "same wide seed, same run");
}

/// The RAM-filesystem quota is passed explicitly, reported in every
/// result, and actually enforced in the guest.
#[test]
fn the_fs_quota_is_reported_and_enforced() {
    let Some(runner) = runner() else { return };
    let plain = runner
        .run_js("console.log(1);", &RunOptions::default())
        .expect("run");
    assert_eq!(plain.fs_quota_mb, 64, "tardigrade's default, stated");

    let tight = runner.with_fs_quota_mb(1);
    let result = tight
        .run_js(
            r#"try { vm.fs.write("/big", "x".repeat(2 * 1024 * 1024)); console.log("wrote"); } catch (e) { console.log("threw"); }"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert_eq!(result.fs_quota_mb, 1);
    assert_eq!(
        result.stdout.trim(),
        "threw",
        "2 MiB must not fit a 1 MiB quota"
    );
}

/// --module is opt-in per run: top-level await parses as a module and
/// is a syntax error as a classic script.
#[test]
fn module_mode_is_opt_in() {
    let Some(runner) = runner() else { return };
    if !runner.capabilities().module {
        eprintln!("SKIP: tardigrade binary has no --module");
        return;
    }
    let code = "const v = await Promise.resolve(42); console.log(v);";
    let as_script = runner.run_js(code, &RunOptions::default()).expect("run");
    assert_eq!(as_script.status, RunStatus::Error);

    let opts = RunOptions {
        module: true,
        ..RunOptions::default()
    };
    let as_module = runner.run_js(code, &opts).expect("run");
    assert_eq!(
        as_module.status,
        RunStatus::Ok,
        "stderr: {:?}",
        as_module.stderr
    );
    assert_eq!(as_module.stdout.trim(), "42");
}

/// A cancelled run is SIGKILLed promptly and reported as cancelled, not
/// as a timeout: the flag is set 100 ms into a run allowed a full minute.
#[test]
fn a_cancelled_run_stops_promptly() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let Some(runner) = runner() else { return };
    let cancel = Arc::new(AtomicBool::new(false));
    let setter = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
        })
    };
    let opts = RunOptions {
        timeout_ms: 60_000,
        ..RunOptions::default()
    };
    let result = runner
        .run_js_cancellable("for (;;) {}", &opts, Some(&cancel))
        .expect("run");
    let _ = setter.join();
    assert_eq!(result.status, RunStatus::Cancelled);
    assert!(
        result.duration_ms < 10_000,
        "cancellation must be prompt, took {} ms",
        result.duration_ms
    );
}
