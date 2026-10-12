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

use tardigrade_runner::{RunOptions, RunStatus};

mod common;
use common::runner;
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

/// A guest can abort its own process: ICU data is missing, so Intl date
/// formatting is a fatal V8 error rather than an exception (exit 133). That is a
/// known tardigrade bug, documented in its KNOWN_BUGS.md, but the runner must
/// contain the damage: report the abort as an error and keep serving.
#[test]
fn a_guest_abort_is_contained_and_the_runner_keeps_serving() {
    let Some(runner) = runner() else { return };
    let aborted = runner
        .run_js(
            r#"console.log(new Intl.DateTimeFormat("en-GB").format(new Date(0)));"#,
            &RunOptions::default(),
        )
        .expect("the runner itself must survive");
    assert_eq!(aborted.status, RunStatus::Error, "an abort is not success");
    assert_eq!(
        aborted.exit_code, None,
        "the guest dies by signal, not exit code"
    );

    let after = runner
        .run_js("console.log(6 * 7);", &RunOptions::default())
        .expect("and the runner must still work");
    assert_eq!(after.status, RunStatus::Ok);
    assert_eq!(after.stdout.trim(), "42");
}

/// The virtual clock is not silent any more: the result says which clock ran and
/// what its Date timeline started at. epoch_ms pins a realistic base without
/// giving up reproducibility, and clock real reaches wall-clock time.
#[test]
fn the_time_base_is_reported_and_controllable() {
    let Some(runner) = runner() else { return };

    let default = runner
        .run_js("console.log(Date.now());", &RunOptions::default())
        .expect("run");
    assert_eq!(default.clock, "virtual");
    assert_eq!(default.epoch_ms, None);
    assert_eq!(
        default.stdout.trim(),
        "0",
        "virtual time starts at the epoch"
    );

    let pinned = runner
        .run_js(
            "console.log(new Date().toISOString());",
            &RunOptions {
                epoch_ms: Some(1_791_763_200_000),
                ..RunOptions::default()
            },
        )
        .expect("run");
    assert_eq!(pinned.epoch_ms, Some(1_791_763_200_000));
    assert_eq!(pinned.stdout.trim(), "2026-10-12T00:00:00.000Z");

    let real = runner
        .run_js(
            "console.log(Date.now() > 1700000000000);",
            &RunOptions {
                clock: "real".to_owned(),
                ..RunOptions::default()
            },
        )
        .expect("run");
    assert_eq!(real.stdout.trim(), "true", "real time is wall-clock");
}

/// epoch_ms only means anything under the virtual clock, and past the ECMAScript
/// maximum it is a usage error, refused before anything is spawned.
#[test]
fn epoch_ms_is_validated_before_the_guest_starts() {
    let Some(runner) = runner() else { return };
    let wrong_clock = runner.run_js(
        "1+1;",
        &RunOptions {
            clock: "real".to_owned(),
            epoch_ms: Some(5),
            ..RunOptions::default()
        },
    );
    assert!(
        wrong_clock.is_err(),
        "epoch_ms with clock real must be refused"
    );
    assert!(wrong_clock.unwrap_err().contains("needs the virtual clock"));

    let too_far = runner.run_js(
        "1+1;",
        &RunOptions {
            epoch_ms: Some(9_007_199_254_740_992),
            ..RunOptions::default()
        },
    );
    assert!(
        too_far.is_err(),
        "past the ECMAScript maximum must be refused"
    );
}

/// A V8 abort comes back with an explanation appended, because a bare backtrace
/// tells an agent nothing it can act on. Ordinary failures are left alone.
#[test]
fn an_abort_is_explained_and_ordinary_errors_are_not() {
    let Some(runner) = runner() else { return };
    let aborted = runner
        .run_js(
            r#"console.log(new Intl.DateTimeFormat("en-GB").format(new Date(0)));"#,
            &RunOptions::default(),
        )
        .expect("run");
    assert!(
        aborted.stderr.contains("known ICU bug"),
        "an abort must be explained, stderr was {:?}",
        aborted.stderr
    );

    let syntax = runner
        .run_js("const x = ;", &RunOptions::default())
        .expect("run");
    assert!(
        syntax.stderr.contains("SyntaxError") && !syntax.stderr.contains("known ICU bug"),
        "ordinary errors keep their own trace, stderr was {:?}",
        syntax.stderr
    );
}

/// There is no crypto API, so an agent asked to hash has to write the hash. This
/// proves that works, against published SHA-256 vectors, so the gap is a
/// workaround rather than a wall.
#[test]
fn hashing_is_possible_without_a_crypto_api() {
    let Some(runner) = runner() else { return };
    let result = runner
        .run_js(SHA256_GUEST, &RunOptions::default())
        .expect("run");
    assert_eq!(
        result.status,
        RunStatus::Ok,
        "stderr was {:?}",
        result.stderr
    );
    assert!(
        result
            .stdout
            .contains("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        "the empty-input SHA-256 vector, stdout was {:?}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "the abc SHA-256 vector, stdout was {:?}",
        result.stdout
    );
    assert!(
        result.duration_ms < 10_000,
        "hashing a MiB took {} ms",
        result.duration_ms
    );
}

/// SHA-256 in guest JavaScript, with no host help, over three inputs.
const SHA256_GUEST: &str = r##"
function sha256(bytes) {
  const K = [0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
  const H = [0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
  const rr = (x, n) => ((x >>> n) | (x << (32 - n))) >>> 0;
  const bitLen = bytes.length * 8;
  const padded = new Uint8Array((((bytes.length + 9) >> 6) + 1) << 6);
  padded.set(bytes);
  padded[bytes.length] = 0x80;
  const dv = new DataView(padded.buffer);
  dv.setUint32(padded.length - 4, bitLen >>> 0, false);
  dv.setUint32(padded.length - 8, Math.floor(bitLen / 4294967296), false);
  const w = new Uint32Array(64);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i++) w[i] = dv.getUint32(off + i * 4, false);
    for (let i = 16; i < 64; i++) {
      const s0 = rr(w[i-15], 7) ^ rr(w[i-15], 18) ^ (w[i-15] >>> 3);
      const s1 = rr(w[i-2], 17) ^ rr(w[i-2], 19) ^ (w[i-2] >>> 10);
      w[i] = (w[i-16] + s0 + w[i-7] + s1) >>> 0;
    }
    let a = H[0], b = H[1], c = H[2], d = H[3], e = H[4], f = H[5], g = H[6], h = H[7];
    for (let i = 0; i < 64; i++) {
      const S1 = rr(e, 6) ^ rr(e, 11) ^ rr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + K[i] + w[i]) >>> 0;
      const S0 = rr(a, 2) ^ rr(a, 13) ^ rr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      h = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0;
    }
    H[0] = (H[0] + a) >>> 0; H[1] = (H[1] + b) >>> 0; H[2] = (H[2] + c) >>> 0; H[3] = (H[3] + d) >>> 0;
    H[4] = (H[4] + e) >>> 0; H[5] = (H[5] + f) >>> 0; H[6] = (H[6] + g) >>> 0; H[7] = (H[7] + h) >>> 0;
  }
  return H.map((x) => x.toString(16).padStart(8, "0")).join("");
}
const enc = (s) => new TextEncoder().encode(s);
const big = new Uint8Array(1024 * 1024);
for (let i = 0; i < big.length; i++) big[i] = i & 0xff;
console.log(JSON.stringify({
  empty: sha256(enc("")),
  abc: sha256(enc("abc")),
  mib: sha256(big).slice(0, 16),
}));
"##;
