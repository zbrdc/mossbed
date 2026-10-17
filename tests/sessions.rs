// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Session tests against the real tardigrade binary's --serve mode.
//! They skip when the binary is absent, like the rest of the suite, and
//! also when the binary predates --serve.

use mossbed::RunOptions;

mod common;
use common::runner;

fn session_opts() -> RunOptions {
    RunOptions {
        timeout_ms: 1_000,
        ..RunOptions::default()
    }
}

/// The defining property of a session, and the opposite of run_js:
/// state set by one eval is still there for the next.
#[test]
fn state_survives_between_evals() {
    let Some(runner) = runner() else { return };
    if !runner.capabilities().serve {
        eprintln!("SKIP: tardigrade binary has no --serve");
        return;
    }
    let mut session = runner
        .open_session(Some("globalThis.base = 40;"), &session_opts(), 60_000)
        .expect("open session");
    let first = session.eval("base + 1").expect("eval");
    assert_eq!(first.reply, "41", "entry state must be visible");
    // The bump answers with a value the read below cannot also produce, so
    // a reply served from the wrong command fails the test instead of
    // passing by coincidence.
    let bumped = session
        .eval(r#"(() => { globalThis.base += 1; return "bumped" })()"#)
        .expect("eval");
    assert_eq!(
        bumped.reply, r#""bumped""#,
        "each eval answers its own command"
    );
    let second = session.eval("base").expect("eval");
    assert_eq!(second.reply, "41", "the increment must reach a later eval");

    // Files persist too, in the same RAM filesystem.
    session
        .eval(r#"(() => { vm.fs.write("/note", "still here"); return 1 })()"#)
        .expect("eval write");
    let read = session.eval(r#"vm.fs.read("/note")"#).expect("eval read");
    assert_eq!(read.reply, r#""still here""#);

    let closed = session.close();
    assert_eq!(closed.exit_code, Some(0));
}

/// A guest error is a reply, not a dead session: the error object comes
/// back and later evals still work.
#[test]
fn a_guest_error_is_a_reply_not_the_end() {
    let Some(runner) = runner() else { return };
    if !runner.capabilities().serve {
        eprintln!("SKIP: tardigrade binary has no --serve");
        return;
    }
    let mut session = runner
        .open_session(None, &session_opts(), 60_000)
        .expect("open session");
    let bad = session
        .eval("(() => { throw new Error('boom') })()")
        .expect("eval");
    assert!(
        bad.reply.contains("$error") && bad.reply.contains("boom"),
        "reply was {:?}",
        bad.reply
    );
    let syntax = session.eval("let x = ;").expect("eval");
    assert!(
        syntax.reply.contains("$error"),
        "reply was {:?}",
        syntax.reply
    );
    let after = session.eval("6 * 7").expect("eval after errors");
    assert_eq!(after.reply, "42");
    let _ = session.close();
}

/// The per-command timeout is what keeps one bad eval from wedging the
/// session: an infinite loop answers "command timed out" and the next
/// eval proceeds.
#[test]
fn a_command_timeout_leaves_the_session_usable() {
    let Some(runner) = runner() else { return };
    if !runner.capabilities().serve {
        eprintln!("SKIP: tardigrade binary has no --serve");
        return;
    }
    let opts = RunOptions {
        timeout_ms: 200,
        ..RunOptions::default()
    };
    let mut session = runner
        .open_session(None, &opts, 60_000)
        .expect("open session");
    let stuck = session.eval("(() => { for (;;) {} })()").expect("eval");
    assert!(
        stuck.reply.contains("command timed out"),
        "reply was {:?}",
        stuck.reply
    );
    let after = session.eval("2 + 2").expect("eval after timeout");
    assert_eq!(after.reply, "4");
    let _ = session.close();
}

/// The serve protocol is one expression per line; the runner refuses
/// multi-line commands outright instead of letting them be read as
/// several commands.
#[test]
fn multi_line_commands_are_refused() {
    let Some(runner) = runner() else { return };
    if !runner.capabilities().serve {
        eprintln!("SKIP: tardigrade binary has no --serve");
        return;
    }
    let mut session = runner
        .open_session(None, &session_opts(), 60_000)
        .expect("open session");
    let refused = session
        .eval("1 + 1\n2 + 2")
        .expect_err("two lines must refuse");
    assert!(refused.contains("one line"), "err was {refused:?}");
    // And the refusal did not consume anything: the session still works.
    assert_eq!(session.eval("3 + 3").expect("eval").reply, "6");
    let _ = session.close();
}

/// With dump requested at open, closing returns the RAM snapshot — the
/// same handoff run_js offers, at session granularity.
#[test]
fn close_returns_the_dump_when_requested() {
    let Some(runner) = runner() else { return };
    if !runner.capabilities().serve {
        eprintln!("SKIP: tardigrade binary has no --serve");
        return;
    }
    let opts = RunOptions {
        dump: true,
        ..session_opts()
    };
    let mut session = runner
        .open_session(None, &opts, 60_000)
        .expect("open session");
    session
        .eval(r#"(() => { vm.fs.write("/kept", "from session"); return 1 })()"#)
        .expect("eval");
    let closed = session.close();
    let dump = closed.dump.expect("dump was requested");
    assert!(dump.contains("/kept"), "dump was {dump:?}");
}
