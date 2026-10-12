// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Agent-shaped usage: what a model that writes JavaScript and reads the result
//! actually experiences.
//!
//! The other suites test the runner and the sandbox boundary. These test the
//! loop an agent runs: write a script, read what came back, decide what to do
//! next. That loop lives or dies on three things a microbenchmark never touches,
//! so each has a test here: whether an error names the fault and its location,
//! whether a realistic payload fits, and whether anything can be assumed to
//! survive from one call to the next.
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

/// The single most important behaviour for an agent: a bad script must come
/// back with the kind of error and a location, so the next attempt can be a
/// correction rather than a guess. Measured: exit 1 and a stderr line naming the
/// file, line and column.
#[test]
fn a_syntax_error_names_the_fault_and_its_location() {
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js("const x = ;", &RunOptions::default())
        .expect("run");
    assert_eq!(result.status, RunStatus::Error);
    assert_eq!(result.exit_code, Some(1));
    assert!(
        result.stderr.contains("SyntaxError") && result.stderr.contains(".js:1"),
        "stderr must locate the fault, was {:?}",
        result.stderr
    );
}

/// An agent's first instinct is Node: require, process, fetch. Each must fail by
/// name, so the model learns the environment instead of retrying the same call.
#[test]
fn node_and_browser_assumptions_fail_by_name() {
    let Some(runner) = runner() else { return };
    for (code, expected) in [
        (r#"require("fs");"#, "require is not defined"),
        ("process.env;", "process is not defined"),
        (r#"fetch("http://example.com");"#, "fetch is not defined"),
    ] {
        let result = runner.run_js(code, &RunOptions::default()).expect("run");
        assert_eq!(result.status, RunStatus::Error, "for {code}");
        assert!(
            result.stderr.contains(expected),
            "for {code}: stderr was {:?}",
            result.stderr
        );
    }
}

/// Agents embed their data in the script, because nothing else crosses the
/// boundary. A payload the size of a real dataset must still run.
#[test]
fn a_large_embedded_payload_runs() {
    let Some(runner) = runner() else { return };
    let items: Vec<String> = (0..4000)
        .map(|i| format!(r#"{{id:{i},name:"item-{i}"}}"#))
        .collect();
    let code = format!(
        "const data = [{}];
console.log(data.length, data[3999].name);",
        items.join(",")
    );
    assert!(code.len() > 100_000, "payload should exceed 100 KB");
    let result = runner.run_js(&code, &RunOptions::default()).expect("run");
    assert_eq!(result.status, RunStatus::Ok);
    assert_eq!(result.stdout.trim(), "4000 item-3999");
}

/// Text an agent prints is often not ASCII.
#[test]
fn unicode_survives_the_round_trip() {
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js(
            r#"console.log(JSON.stringify({emoji:"🧪🐻", cjk:"水熊虫", accent:"café"}));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert!(
        result.stdout.contains(r#""emoji":"🧪🐻""#)
            && result.stdout.contains(r#""cjk":"水熊虫""#)
            && result.stdout.contains(r#""accent":"café""#),
        "stdout was {:?}",
        result.stdout
    );
}

/// Every call is a fresh process, so an agent cannot carry state across calls:
/// no globals, no arguments, no stdin. This is the assumption most likely to
/// break a multi-step agent task, so it is pinned rather than assumed.
#[test]
fn nothing_survives_between_calls() {
    let Some(runner) = runner() else { return };
    let first = runner
        .run_js(
            r#"globalThis.carried = "value"; vm.fs.write("/carried.txt", "value");
console.log(JSON.stringify({set: globalThis.carried}));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert!(first.stdout.contains(r#""set":"value""#));

    let second = runner
        .run_js(
            r#"console.log(JSON.stringify({
  global: typeof globalThis.carried,
  file: vm.fs.exists("/carried.txt"),
  args: vm.args.length,
  stdin: JSON.stringify(vm.stdin.read()),
}));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert!(
        second.stdout.contains(r#""global":"undefined""#)
            && second.stdout.contains(r#""file":false"#)
            && second.stdout.contains(r#""args":0"#),
        "nothing may carry over, stdout was {:?}",
        second.stdout
    );
}
