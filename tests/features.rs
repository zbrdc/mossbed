//! New-capability tests: identity plumbing, run manifests, run_bundle
//! validation, and the orphan cascade (a killed mossbed leaves no
//! tardigrade or net-sidecar descendants behind).

mod common;

use mossbed::{RunOptions, RunStatus};

fn opts_with(manifest: bool) -> RunOptions {
    RunOptions {
        manifest,
        ..RunOptions::default()
    }
}

#[test]
fn identity_is_probed_from_the_binary() {
    let Some(runner) = common::runner() else {
        return;
    };
    assert!(
        runner.engine().is_some(),
        "the wrapped tardigrade advertises --identity"
    );
}

#[test]
fn run_manifest_comes_back_parsed() {
    let Some(runner) = common::runner() else {
        return;
    };
    let result = runner
        .run_js("console.log('m');", &opts_with(true))
        .unwrap();
    assert_eq!(result.status, RunStatus::Ok);
    let manifest = result.manifest.expect("manifest was requested");
    assert_eq!(manifest["format"], "tardigrade.run-provenance");
    assert!(manifest["inputs"]["entry"]["sha256"].is_string());
}

fn sha256_hex(bytes: &[u8]) -> String {
    use ring::digest::{SHA256, digest};
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn bundle_json(engine: &str, source: &str) -> String {
    serde_json::json!({
        "format": "tardigrade.deploy-bundle",
        "version": 1,
        "runtime": {"v8": engine},
        "entry": {
            "path": "app.js",
            "source": source,
            "sha256": sha256_hex(source.as_bytes()),
        }
    })
    .to_string()
}

#[test]
fn run_bundle_executes_a_valid_artifact() {
    let Some(runner) = common::runner() else {
        return;
    };
    let engine = runner.engine().expect("engine probed").to_owned();
    let bundle = bundle_json(&engine, "console.log('from bundle');");
    let result = runner.run_bundle(&bundle, &opts_with(false)).unwrap();
    assert_eq!(result.status, RunStatus::Ok, "{}", result.stderr);
    assert_eq!(result.stdout, "from bundle\n");
}

#[test]
fn run_bundle_refuses_a_tampered_artifact() {
    let Some(runner) = common::runner() else {
        return;
    };
    let engine = runner.engine().expect("engine probed").to_owned();
    let bundle = bundle_json(&engine, "console.log('x');")
        .replace("console.log('x');", "console.log('swapped');");
    let result = runner.run_bundle(&bundle, &opts_with(false)).unwrap();
    assert_eq!(result.exit_code, Some(2), "{}", result.stderr);
    assert_eq!(result.stdout, "", "no guest output after refusal");
}

#[test]
fn killing_mossbed_leaves_no_guest_descendants() {
    // The orphan cascade, end to end: with a net request in flight,
    // SIGKILLing mossbed must end the tardigrade guest (stdin EOF) and
    // its net-sidecar (PDEATHSIG plus the group kill) within seconds.
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::{Command, Stdio};

    let bin = std::env::var("TARDIGRADE_BIN").unwrap_or_else(|_| "tardigrade".into());
    if !std::path::Path::new(&bin).is_file() {
        eprintln!("SKIP: tardigrade binary not found");
        return;
    }

    // A listener that accepts and never answers: the sidecar request
    // stays in flight for the whole test.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hold = std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 256];
            loop {
                match conn.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_mossbed"))
        .env("TARDIGRADE_BIN", &bin)
        .env(
            "MOSSBED_ALLOW_CALL",
            format!("slow=http://127.0.0.1:{port}/echo"),
        )
        .env("MOSSBED_NET_ALLOW_PRIVATE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mossbed");

    let mut stdin = child.stdin.take().unwrap();
    let _out = child.stdout.take().unwrap();
    let send = |stdin: &mut std::process::ChildStdin, msg: &serde_json::Value| {
        let _ = stdin.write_all(msg.to_string().as_bytes());
        let _ = stdin.write_all(b"\n");
        let _ = stdin.flush();
    };
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "t", "version": "0"}
            }
        }),
    );
    send(
        &mut stdin,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {
                "name": "run_js",
                "arguments": {
                    "code": "vm.net.request('slow','x').then(()=>0,()=>0)",
                    "timeout_ms": 3000,
                    "clock": "real"
                }
            }
        }),
    );

    // Close our end of the command pipe: the guest only sees EOF (and
    // exits, taking its sidecar with it) once every write end is gone.
    drop(stdin);

    // Let the guest spawn and its sidecar connect.
    std::thread::sleep(std::time::Duration::from_millis(1500));

    // Hard-kill mossbed exactly like a crashed host would.
    let pid = child.id();
    let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
    let _ = child.wait();

    // No descendant of the killed server may survive: no guest running
    // our tardigrade binary, no net-sidecar anywhere.
    // One-shot guests do not read stdin, so a crashed mossbed leaves them
    // bounded by their own --timeout (plus grace), not dead instantly
    // like serve sessions; the cascade must still complete within that.
    // One-shot guests do not read stdin, so a crashed mossbed leaves them
    // bounded by their own --timeout (plus grace), not dead instantly
    // like serve sessions. The sidecar is additionally subject to
    // tardigrade FU-37 (pdeathsig defeated under --sandbox): it exits on
    // its net-timeout instead, so the cascade bound here is timeout +
    // net-timeout + margin. Tighten when FU-37 is fixed.
    // Bounded by tardigrade FU-37: under --sandbox the sidecar ignores its
    // armed parent-death signal and only exits at the full net timeout
    // (MOSSBED_NET_TIMEOUT, default 30 s) after the guest is gone. The
    // guest itself dies at its --timeout (3 s). Tighten when FU-37 is fixed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
    loop {
        let mut survivors = 0;
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{name}/cmdline")) else {
                continue;
            };
            let argv: Vec<&str> = cmdline.split('\u{0}').filter(|s| !s.is_empty()).collect();
            // Only processes actually executing the guest binary count: the
            // wrapper shells that launched this test also mention the path
            // in their command text and must not count as survivors.
            if argv.first() == Some(&bin.as_str()) {
                survivors += 1;
            }
        }
        if survivors == 0 {
            break;
        }
        if std::time::Instant::now() >= deadline {
            for entry in std::fs::read_dir("/proc").unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if let Ok(cl) = std::fs::read_to_string(format!("/proc/{name}/cmdline")) {
                    let argv: Vec<&str> = cl.split('\u{0}').filter(|s| !s.is_empty()).collect();
                    if argv.first() == Some(&bin.as_str()) {
                        eprintln!("SURVIVOR {name}: {argv:?}");
                    }
                }
            }
            panic!("' + '{survivors} guest descendants survived mossbed' + chr(39) + 's death");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    drop(hold);
}
