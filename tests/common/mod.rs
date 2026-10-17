// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Shared test scaffolding.
//!
//! Every suite that needs the real tardigrade binary skips itself when the
//! binary is absent, which locally is convenient and in CI is dangerous: a suite
//! of skipped tests reports success while measuring nothing. Setting
//! TARDIGRADE_REQUIRE_TESTS turns that skip into a failure, so a release check
//! can insist that the V8-backed tests actually ran.

use mossbed::{Runner, resolve_bin};

/// A checked runner, or None when the binary is absent and skipping is allowed.
pub fn runner() -> Option<Runner> {
    let bin = resolve_bin(None);
    if !bin.is_file() {
        let msg = format!(
            "tardigrade binary not found at {} (set TARDIGRADE_BIN to override)",
            bin.display()
        );
        if std::env::var_os("TARDIGRADE_REQUIRE_TESTS").is_some() {
            panic!("TARDIGRADE_REQUIRE_TESTS is set but {msg}");
        }
        eprintln!("SKIP: {msg}");
        return None;
    }
    let mut runner = Runner::new(bin).expect("runner config");
    runner.sandbox_check().expect("tardigrade --sandbox-check");
    Some(runner)
}
