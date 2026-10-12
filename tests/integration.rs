//! Integration tests against the real tardigrade binary (default
//! ../tardigrade/target/release/tardigrade, or TARDIGRADE_BIN).
//! Each test skips with a message when the binary is not found.

use tardigrade_runner::{resolve_bin, RunOptions, RunStatus, Runner};

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
