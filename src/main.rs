// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Minimal hand-rolled MCP stdio server: JSON-RPC 2.0, one line per message,
//! no SDK dependency. Exposes exactly one tool: `run_js`.

use std::io::{self, BufRead, Write};

use serde::Deserialize;
use serde_json::{Value, json};

use tardigrade_runner::{
    DRAIN_CAP_BYTES, RunOptions, RunResult, Runner, TRUNCATED_MARKER, resolve_bin,
};

#[derive(Deserialize)]
struct RunJsParams {
    code: String,
    timeout_ms: Option<u32>,
    seed: Option<u32>,
    clock: Option<String>,
}

fn main() {
    let flag = std::env::args()
        .skip(1)
        .find_map(|a| a.strip_prefix("--tardigrade=").map(|p| p.to_owned()));
    let bin = resolve_bin(flag.as_deref());

    let mut runner = match Runner::new(bin) {
        Ok(runner) => runner,
        Err(msg) => {
            eprintln!("tardigrade-runner: {msg}");
            std::process::exit(2);
        }
    };

    // Fail closed: check once at startup; a failure keeps the server alive
    // but refuses every run with a clear error.
    let gate = runner.sandbox_check();
    if let Err(msg) = &gate {
        eprintln!("tardigrade-runner: refusing all runs: {msg}");
    }

    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    for line in stdin.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // A closed client pipe must end the server, not spin it forever.
        if handle_line(&line, &runner, &gate, &mut stdout).is_err() || stdout.flush().is_err() {
            break;
        }
    }
    // The loop is over, so no run can be in flight: drop the per-process
    // directory rather than leaving an empty one behind for every lifetime.
    tardigrade_runner::cleanup_script_dir();
}

fn handle_line(
    line: &str,
    runner: &Runner,
    gate: &Result<(), String>,
    out: &mut impl Write,
) -> io::Result<()> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(msg) => msg,
        Err(e) => {
            return write_json(
                out,
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            );
        }
    };
    // Notifications (no id) are ignored.
    let Some(id) = msg.get("id").cloned() else {
        return Ok(());
    };
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reply = match method {
        "initialize" => Ok(initialize()),
        "tools/list" => Ok(json!({"tools": [tool_spec()]})),
        "tools/call" => call_tool(msg.get("params"), runner, gate),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    match reply {
        Ok(result) => write_json(out, json!({"jsonrpc": "2.0", "id": id, "result": result})),
        Err((code, message)) => write_json(
            out,
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        ),
    }
}

fn initialize() -> Value {
    json!({
        // Answer with a version this server implements, never echo the
        // client's proposal (even a bogus future one) verbatim.
        "protocolVersion": "2024-11-05",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "tardigrade-runner", "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tool_spec() -> Value {
    json!({
        "name": "run_js",
        "description": "Run JavaScript in the sandboxed tardigrade V8 runtime (Landlock + seccomp, \
    RAM filesystem, no host APIs). Returns JSON {status, exit_code, stdout, stderr, duration_ms, memory_cap_mb}.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "code": {"type": "string", "description": "JavaScript source to run"},
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 3600000},
                "seed": {"type": "integer", "minimum": 0},
                "clock": {"type": "string", "enum": ["virtual", "real"]}
            },
            "required": ["code"]
        }
    })
}

fn call_tool(
    params: Option<&Value>,
    runner: &Runner,
    gate: &Result<(), String>,
) -> Result<Value, (i32, String)> {
    let params = params.cloned().unwrap_or(Value::Null);
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != "run_js" {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    let args: RunJsParams =
        serde_json::from_value(params.get("arguments").cloned().unwrap_or(Value::Null))
            .map_err(|e| (-32602, format!("invalid arguments: {e}")))?;

    let error_text =
        |text: String| Ok(json!({"content": [{"type": "text", "text": text}], "isError": true}));
    if let Err(msg) = gate {
        return error_text(format!("refused: tardigrade sandbox check failed: {msg}"));
    }

    let defaults = RunOptions::default();
    let opts = RunOptions {
        timeout_ms: args.timeout_ms.unwrap_or(defaults.timeout_ms),
        seed: args.seed.unwrap_or(defaults.seed),
        clock: args.clock.unwrap_or(defaults.clock),
    };
    match runner.run_js(&args.code, &opts) {
        Ok(result) => Ok(json!({
            "content": [{"type": "text", "text": serde_json::to_string(&truncate(result)).expect("serialize result")}]
        })),
        Err(msg) => error_text(msg),
    }
}

/// Caps stdout/stderr at 64 KiB each, cut at a UTF-8 char boundary.
fn truncate(mut result: RunResult) -> RunResult {
    result.stdout = truncate_str(&result.stdout);
    result.stderr = truncate_str(&result.stderr);
    result
}

fn truncate_str(s: &str) -> String {
    if s.len() <= DRAIN_CAP_BYTES {
        return s.to_owned();
    }
    let mut cut = DRAIN_CAP_BYTES;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{}", &s[..cut], TRUNCATED_MARKER)
}

fn write_json(out: &mut impl Write, value: Value) -> io::Result<()> {
    writeln!(out, "{value}")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenPipe;
    impl Write for BrokenPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
    }

    #[test]
    fn write_errors_reach_the_loop() {
        let runner = Runner::new(std::path::PathBuf::from("unused")).expect("runner config");
        let gate: Result<(), String> = Ok(());
        // Malformed JSON forces the parse-error reply, which must hit the
        // broken writer and propagate instead of being swallowed.
        assert!(handle_line("{", &runner, &gate, &mut BrokenPipe).is_err());
    }

    #[test]
    fn initialize_answers_supported_protocol_version() {
        assert_eq!(initialize()["protocolVersion"].as_str(), Some("2024-11-05"));
    }

    #[test]
    fn truncate_cuts_on_char_boundary_and_marks() {
        // 70_000 'é' bytes = 140_000 bytes, boundary is not a char edge.
        let long = "é".repeat(70_000);
        let cut = truncate_str(&long);
        assert_eq!(cut.len(), DRAIN_CAP_BYTES + TRUNCATED_MARKER.len());
        assert!(cut.ends_with(TRUNCATED_MARKER));
        let short = "x".repeat(DRAIN_CAP_BYTES);
        assert_eq!(truncate_str(&short), short);
    }
}
