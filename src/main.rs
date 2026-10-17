// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Minimal hand-rolled MCP stdio server: JSON-RPC 2.0, one line per message,
//! no SDK dependency. Tools: `run_js` for one-shot runs, plus
//! `session_open`/`session_eval`/`session_close` over tardigrade's --serve
//! console mode when the binary offers it. Requests dispatch on threads
//! (the library is concurrency-safe), so a long run no longer blocks the
//! server, and `notifications/cancelled` can actually cancel one.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use mossbed::{
    DRAIN_CAP_BYTES, RunOptions, RunResult, Runner, Session, TRUNCATED_MARKER, resolve_bin,
};

/// Protocol versions this server implements, newest first. Initialize
/// answers the client's version when it is one of these, else the newest
/// implemented — never an echo of a version it does not speak.
const SUPPORTED_PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Whole-session bound when session_open does not say (tardigrade --timeout).
const DEFAULT_SESSION_TTL_MS: u32 = 300_000;

/// Operator settings for session housekeeping, mirroring the library's
/// rule that ceilings are never tool arguments.
const DEFAULT_MAX_SESSIONS: usize = 8;
const DEFAULT_SESSION_IDLE_MS: u64 = 300_000;

const LANGUAGE_GUIDANCE: &str = "Transpile TypeScript to JavaScript first. For C/C++/Rust, \
    compile to WebAssembly and load it from JavaScript. Supply any required imports or WASI shim. \
    No compilers or WASI runtime are bundled. Native executables cannot run in the guest.";

#[derive(Deserialize)]
struct RunJsParams {
    code: String,
    timeout_ms: Option<u32>,
    seed: Option<u64>,
    clock: Option<String>,
    epoch_ms: Option<u64>,
    preload: Option<String>,
    dump: Option<bool>,
    args: Option<Vec<String>>,
    module: Option<bool>,
}

#[derive(Deserialize)]
struct SessionOpenParams {
    entry_code: Option<String>,
    timeout_ms: Option<u32>,
    ttl_ms: Option<u32>,
    seed: Option<u64>,
    clock: Option<String>,
    epoch_ms: Option<u64>,
    preload: Option<String>,
    dump: Option<bool>,
    args: Option<Vec<String>>,
    module: Option<bool>,
}

#[derive(Deserialize)]
struct SessionEvalParams {
    session_id: String,
    code: String,
}

#[derive(Deserialize)]
struct SessionCloseParams {
    session_id: String,
}

struct Managed {
    session: Arc<Mutex<Option<Session>>>,
    last_used: Instant,
}

struct State {
    runner: Arc<Runner>,
    gate: Result<(), String>,
    sessions: Mutex<HashMap<String, Managed>>,
    /// In-flight run_js cancellations, keyed by the JSON-RPC request id.
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    session_seq: AtomicU64,
    max_sessions: usize,
    session_idle: Duration,
    broken: AtomicBool,
}

fn env_usize(name: &str, default: usize) -> Result<usize, String> {
    match std::env::var(name) {
        Ok(v) => v.parse().map_err(|e| format!("bad {name} {v:?}: {e}")),
        Err(_) => Ok(default),
    }
}

fn main() {
    let flag = std::env::args()
        .skip(1)
        .find_map(|a| a.strip_prefix("--tardigrade=").map(|p| p.to_owned()));
    let bin = resolve_bin(flag.as_deref());

    let mut runner = match Runner::new(bin) {
        Ok(runner) => runner,
        Err(msg) => {
            eprintln!("mossbed: {msg}");
            std::process::exit(2);
        }
    };
    let (max_sessions, session_idle) = match (
        env_usize("MOSSBED_MAX_SESSIONS", DEFAULT_MAX_SESSIONS),
        env_usize("MOSSBED_SESSION_IDLE_MS", DEFAULT_SESSION_IDLE_MS as usize),
    ) {
        (Ok(max), Ok(idle)) => (max, Duration::from_millis(idle as u64)),
        (Err(msg), _) | (_, Err(msg)) => {
            eprintln!("mossbed: {msg}");
            std::process::exit(2);
        }
    };

    // Fail closed: check once at startup; a failure keeps the server alive
    // but refuses every run with a clear error.
    let gate = runner.sandbox_check();
    if let Err(msg) = &gate {
        eprintln!("mossbed: refusing all runs: {msg}");
    }

    let state = Arc::new(State {
        runner: Arc::new(runner),
        gate,
        sessions: Mutex::new(HashMap::new()),
        cancels: Mutex::new(HashMap::new()),
        session_seq: AtomicU64::new(0),
        max_sessions,
        session_idle,
        broken: AtomicBool::new(false),
    });
    let out: Arc<Mutex<io::Stdout>> = Arc::new(Mutex::new(io::stdout()));

    let stdin = io::stdin().lock();
    let mut handles = Vec::new();
    for line in stdin.lines() {
        if state.broken.load(Ordering::Relaxed) {
            break;
        }
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let state = Arc::clone(&state);
        let out = Arc::clone(&out);
        handles.push(std::thread::spawn(move || {
            if let Some(response) = handle_line(&line, &state) {
                let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                // A closed client pipe must end the server, not spin it.
                if writeln!(out, "{response}").is_err() || out.flush().is_err() {
                    state.broken.store(true, Ordering::Relaxed);
                }
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
    // No run or eval can be in flight now: close every guest and drop the
    // per-process directory rather than leaving an empty one behind.
    close_all_sessions(&state);
    mossbed::cleanup_script_dir();
}

fn close_all_sessions(state: &State) {
    let drained: Vec<Managed> = {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.drain().map(|(_, managed)| managed).collect()
    };
    for managed in drained {
        if let Some(session) = managed
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = session.close();
        }
    }
}

/// Handle one input line; Some is the JSON-RPC response to write, None
/// means the line was a notification and MCP expects silence.
fn handle_line(line: &str, state: &State) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(msg) => msg,
        Err(e) => {
            return Some(
                json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}),
            );
        }
    };
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Notifications (no id) get no reply; cancellation is the one that
    // does something.
    let Some(id) = msg.get("id").cloned() else {
        if method == "notifications/cancelled" {
            if let Some(request_id) = msg.pointer("/params/requestId") {
                let key = request_id.to_string();
                let cancels = state.cancels.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(flag) = cancels.get(&key) {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        }
        return None;
    };
    let reply = match method {
        "initialize" => Ok(initialize(msg.get("params"))),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_specs(state)})),
        "tools/call" => call_tool(msg.get("params"), &id, state),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

fn initialize(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let version = match requested {
        Some(v) if SUPPORTED_PROTOCOLS.contains(&v) => v,
        _ => SUPPORTED_PROTOCOLS[0],
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "mossbed", "version": env!("CARGO_PKG_VERSION")},
    })
}

/// The tool list. Session tools appear only when the probed binary
/// advertises --serve; granted network capability names (never URLs)
/// are stated here so a client knows what the guest may call.
fn tool_specs(state: &State) -> Vec<Value> {
    let names = state.runner.net_capability_names();
    let net_note = if names.is_empty() {
        "The guest has no network access.".to_owned()
    } else {
        format!(
            "The operator granted these network capabilities, callable from the guest as \
             vm.net.request(name, body) -> Promise<{{status, text}}>: {}. No other network \
             access exists; URLs are operator policy and are never visible to the guest.",
            names.join(", ")
        )
    };
    let mut tools = vec![json!({
        "name": "run_js",
        "description": format!("Run JavaScript and WebAssembly in the sandboxed tardigrade V8 runtime \
    (Landlock + seccomp, RAM filesystem, no host APIs). {LANGUAGE_GUIDANCE} \
    Time is VIRTUAL by default: Date.now() is 0 and \
    dates read as 1970, so pass clock: \"real\" for wall-clock time, or epoch_ms \
    for a fixed base. Runs are stateless unless you pass preload/dump: dump returns \
    the RAM filesystem as snapshot JSON in the result's dump field, and preload \
    restores a snapshot before the run, so a workflow can hand state between runs. \
    {net_note} Returns JSON {{status, clock, epoch_ms, exit_code, stdout, stderr, duration_ms, \
    memory_cap_mb, heap_limit_mb, fs_quota_mb, net_capabilities, dump?}} (also as structuredContent)."),
        "inputSchema": {
            "type": "object",
            "properties": {
                "code": {"type": "string", "description": "JavaScript source, including transpiled TypeScript or a WebAssembly loader. Not raw TS/C/C++/Rust source or a native executable."},
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 3600000},
                "seed": {"type": "integer", "minimum": 0},
                "clock": {"type": "string", "enum": ["virtual", "real"]},
                "epoch_ms": {"type": "integer", "minimum": 0,
                    "description": "Start of the Date timeline under a virtual clock, ms since 1970. Needs clock=virtual."},
                "preload": {"type": "string",
                    "description": "Snapshot JSON restored into the RAM filesystem before the run. Merge semantics: snapshot entries overwrite, entries not in the snapshot stay. Use the dump field of an earlier run."},
                "dump": {"type": "boolean",
                    "description": "Capture the RAM filesystem at end of run and return it as the result's dump field (snapshot JSON). Feed it back as preload to continue a workflow."},
                "args": {"type": "array", "items": {"type": "string"},
                    "description": "Arguments handed to the guest after the script path."},
                "module": {"type": "boolean",
                    "description": "Parse the code as an ES module (import/export, top-level await; specifiers resolve inside the RAM filesystem)."}
            },
            "required": ["code"]
        },
        "outputSchema": {"type": "object"}
    })];
    if state.runner.capabilities().serve {
        tools.push(json!({
            "name": "session_open",
            "description": format!("Open a persistent sandboxed JavaScript/WebAssembly session (tardigrade --serve): one guest whose globals, \
        timers and RAM files survive across session_eval calls, unlike stateless run_js. entry_code runs once \
        at startup. {LANGUAGE_GUIDANCE} timeout_ms bounds each eval (the guest replies {{\"$error\":\"command timed out\"}} and the \
        session continues); ttl_ms bounds the whole session. Returns {{session_id, ...}}; close with session_close. \
        Idle sessions are reaped by the operator's settings."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entry_code": {"type": "string", "description": "JavaScript run once at session startup. Can contain transpiled TypeScript or instantiate preloaded WebAssembly."},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 3600000,
                        "description": "Per-eval command timeout (default 10000)"},
                    "ttl_ms": {"type": "integer", "minimum": 1, "maximum": 3600000,
                        "description": "Whole-session lifetime bound (default 300000)"},
                    "seed": {"type": "integer", "minimum": 0},
                    "clock": {"type": "string", "enum": ["virtual", "real"]},
                    "epoch_ms": {"type": "integer", "minimum": 0},
                    "preload": {"type": "string",
                        "description": "Snapshot JSON restored into the RAM filesystem at startup (see run_js)."},
                    "dump": {"type": "boolean",
                        "description": "Return the RAM filesystem snapshot from session_close."},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "module": {"type": "boolean", "description": "Parse entry_code as an ES module."}
                }
            },
            "outputSchema": {"type": "object"}
        }));
        tools.push(json!({
            "name": "session_eval",
            "description": "Evaluate ONE JavaScript expression (one line) in a session from session_open and return its value \
        as JSON. It can call loaded WebAssembly exports. Input is JavaScript, not raw TypeScript or C/C++/Rust source. \
        Statements must be wrapped, e.g. (() => { let x = 1; return x })(). Promises are awaited. A guest \
        error comes back as {ok: false, error} and the session continues; guest console output arrives in stderr. \
        Returns {session_id, ok, value?, error?, stderr, duration_ms}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string"},
                    "code": {"type": "string", "description": "One JavaScript expression, one line. Can call loaded WebAssembly exports."}
                },
                "required": ["session_id", "code"]
            },
            "outputSchema": {"type": "object"}
        }));
        tools.push(json!({
            "name": "session_close",
            "description": "Close a session and reap its guest. Returns {session_id, exit_code, stderr, dump?}; \
        dump is present when the session was opened with dump: true.",
            "inputSchema": {
                "type": "object",
                "properties": {"session_id": {"type": "string"}},
                "required": ["session_id"]
            },
            "outputSchema": {"type": "object"}
        }));
    }
    tools
}

fn error_text(text: String) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

/// A tool result carried both ways: as text for clients that only read
/// content, and as structuredContent for clients that do not want to
/// re-parse a JSON string.
fn structured(value: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": value.to_string()}],
        "structuredContent": value,
    })
}

fn gate_refusal(state: &State) -> Option<Value> {
    match &state.gate {
        Err(msg) => Some(error_text(format!(
            "refused: tardigrade sandbox check failed: {msg}"
        ))),
        Ok(()) => None,
    }
}

fn parse_args<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, (i32, String)> {
    serde_json::from_value(params.get("arguments").cloned().unwrap_or(Value::Null))
        .map_err(|e| (-32602, format!("invalid arguments: {e}")))
}

fn call_tool(
    params: Option<&Value>,
    request_id: &Value,
    state: &State,
) -> Result<Value, (i32, String)> {
    let params = params.cloned().unwrap_or(Value::Null);
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match name {
        "run_js" => Ok(run_js_tool(&params, request_id, state)?),
        "session_open" => Ok(session_open_tool(&params, state)?),
        "session_eval" => Ok(session_eval_tool(&params, state)?),
        "session_close" => Ok(session_close_tool(&params, state)?),
        _ => Err((-32602, format!("unknown tool: {name}"))),
    }
}

fn run_js_tool(params: &Value, request_id: &Value, state: &State) -> Result<Value, (i32, String)> {
    let args: RunJsParams = parse_args(params)?;
    if let Some(refusal) = gate_refusal(state) {
        return Ok(refusal);
    }
    let defaults = RunOptions::default();
    let opts = RunOptions {
        timeout_ms: args.timeout_ms.unwrap_or(defaults.timeout_ms),
        seed: args.seed.unwrap_or(defaults.seed),
        clock: args.clock.unwrap_or(defaults.clock),
        epoch_ms: args.epoch_ms,
        preload: args.preload,
        dump: args.dump.unwrap_or(false),
        args: args.args.unwrap_or_default(),
        module: args.module.unwrap_or(false),
    };
    // Register the cancellation flag under this request's id so a
    // notifications/cancelled can find it; deregister on return.
    let cancel = Arc::new(AtomicBool::new(false));
    let key = request_id.to_string();
    state
        .cancels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key.clone(), Arc::clone(&cancel));
    let outcome = state
        .runner
        .run_js_cancellable(&args.code, &opts, Some(&cancel));
    state
        .cancels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    Ok(match outcome {
        Ok(result) => structured(serde_json::to_value(truncate(result)).expect("serialize result")),
        Err(msg) => error_text(msg),
    })
}

/// Drop sessions idle past the operator's limit. Removing the map entry
/// drops one Arc; the guest is killed when the last Arc (possibly held
/// by an in-flight eval) goes away.
fn reap_idle(state: &State) {
    let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    sessions.retain(|_, managed| managed.last_used.elapsed() <= state.session_idle);
}

fn session_open_tool(params: &Value, state: &State) -> Result<Value, (i32, String)> {
    let args: SessionOpenParams = parse_args(params)?;
    if let Some(refusal) = gate_refusal(state) {
        return Ok(refusal);
    }
    reap_idle(state);
    {
        let sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.len() >= state.max_sessions {
            return Ok(error_text(format!(
                "refused: {} sessions already open (operator cap MOSSBED_MAX_SESSIONS); close one first",
                state.max_sessions
            )));
        }
    }
    let defaults = RunOptions::default();
    let opts = RunOptions {
        timeout_ms: args.timeout_ms.unwrap_or(defaults.timeout_ms),
        seed: args.seed.unwrap_or(defaults.seed),
        clock: args.clock.unwrap_or(defaults.clock),
        epoch_ms: args.epoch_ms,
        preload: args.preload,
        dump: args.dump.unwrap_or(false),
        args: args.args.unwrap_or_default(),
        module: args.module.unwrap_or(false),
    };
    let ttl_ms = args.ttl_ms.unwrap_or(DEFAULT_SESSION_TTL_MS);
    match state
        .runner
        .open_session(args.entry_code.as_deref(), &opts, ttl_ms)
    {
        Ok(session) => {
            let id = format!("s-{}", state.session_seq.fetch_add(1, Ordering::Relaxed));
            let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.insert(
                id.clone(),
                Managed {
                    session: Arc::new(Mutex::new(Some(session))),
                    last_used: Instant::now(),
                },
            );
            Ok(structured(json!({
                "session_id": id,
                "command_timeout_ms": opts.timeout_ms,
                "ttl_ms": ttl_ms,
                "clock": opts.clock,
                "epoch_ms": opts.epoch_ms,
                "heap_limit_mb": state.runner.heap_mb(),
                "fs_quota_mb": state.runner.fs_quota_mb(),
                "net_capabilities": state.runner.net_capability_names(),
            })))
        }
        Err(msg) => Ok(error_text(msg)),
    }
}

fn lookup_session(state: &State, session_id: &str) -> Option<Arc<Mutex<Option<Session>>>> {
    reap_idle(state);
    let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
    let managed = sessions.get_mut(session_id)?;
    managed.last_used = Instant::now();
    Some(Arc::clone(&managed.session))
}

fn session_eval_tool(params: &Value, state: &State) -> Result<Value, (i32, String)> {
    let args: SessionEvalParams = parse_args(params)?;
    let Some(slot) = lookup_session(state, &args.session_id) else {
        return Ok(error_text(format!(
            "unknown or expired session: {}",
            args.session_id
        )));
    };
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(session) = slot.as_mut() else {
        return Ok(error_text(format!(
            "session has ended: {}",
            args.session_id
        )));
    };
    match session.eval(&args.code) {
        Ok(reply) => {
            // The reply is one JSON line from the guest: a value, or
            // {"$error": "..."} for a guest-side failure. Both are data,
            // not transport errors; the session continues either way.
            let parsed: Value =
                serde_json::from_str(&reply.reply).unwrap_or(Value::String(reply.reply.clone()));
            let (ok, value, error) = match parsed.get("$error") {
                Some(e) => (false, Value::Null, Some(e.clone())),
                None => (true, parsed, None),
            };
            Ok(structured(json!({
                "session_id": args.session_id,
                "ok": ok,
                "value": value,
                "error": error,
                "stderr": reply.stderr,
                "duration_ms": reply.duration_ms,
            })))
        }
        Err(msg) => {
            // Transport failure: the guest is gone or wedged. Remove the
            // husk so later calls say "unknown" instead of repeating this.
            drop(slot);
            state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&args.session_id);
            Ok(error_text(msg))
        }
    }
}

fn session_close_tool(params: &Value, state: &State) -> Result<Value, (i32, String)> {
    let args: SessionCloseParams = parse_args(params)?;
    let removed = state
        .sessions
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&args.session_id);
    let Some(managed) = removed else {
        return Ok(error_text(format!(
            "unknown or expired session: {}",
            args.session_id
        )));
    };
    let session = managed
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    match session {
        Some(session) => {
            let closed = session.close();
            Ok(structured(json!({
                "session_id": args.session_id,
                "exit_code": closed.exit_code,
                "stderr": closed.stderr,
                "dump": closed.dump,
            })))
        }
        None => Ok(error_text(format!(
            "session has ended: {}",
            args.session_id
        ))),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state() -> State {
        State {
            runner: Arc::new(Runner::new(std::path::PathBuf::from("unused")).expect("config")),
            gate: Ok(()),
            sessions: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            session_seq: AtomicU64::new(0),
            max_sessions: DEFAULT_MAX_SESSIONS,
            session_idle: Duration::from_millis(DEFAULT_SESSION_IDLE_MS),
            broken: AtomicBool::new(false),
        }
    }

    #[test]
    fn malformed_line_gets_a_parse_error_reply() {
        let state = test_state();
        let reply = handle_line("{", &state).expect("parse errors are answered");
        assert_eq!(reply["error"]["code"], json!(-32700));
        // A notification is never answered.
        assert!(
            handle_line(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                &state
            )
            .is_none()
        );
    }

    #[test]
    fn initialize_negotiates_a_supported_protocol_version() {
        let asked = json!({"protocolVersion": "2024-11-05"});
        assert_eq!(
            initialize(Some(&asked))["protocolVersion"].as_str(),
            Some("2024-11-05")
        );
        // A bogus future version is not echoed: answer the newest spoken.
        let bogus = json!({"protocolVersion": "2999-01-01"});
        assert_eq!(
            initialize(Some(&bogus))["protocolVersion"].as_str(),
            Some("2025-06-18")
        );
        assert_eq!(
            initialize(None)["protocolVersion"].as_str(),
            Some("2025-06-18")
        );
    }

    #[test]
    fn session_tools_hidden_without_serve_capability() {
        let state = test_state();
        let tools = tool_specs(&state);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            ["run_js"],
            "unprobed runner must not promise sessions"
        );
    }

    #[test]
    fn tool_discovery_explains_language_preparation() {
        let reply = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            &test_state(),
        )
        .expect("tools/list reply");
        let tool = &reply["result"]["tools"][0];
        assert_eq!(tool["name"], "run_js");
        let description = tool["description"].as_str().expect("description");
        for guidance in [
            "JavaScript",
            "WebAssembly",
            "TypeScript",
            "C/C++/Rust",
            "compile",
            "WASI",
            "Native executables",
        ] {
            assert!(description.contains(guidance), "missing {guidance}");
        }
        assert!(
            tool["inputSchema"]["properties"]["code"]["description"]
                .as_str()
                .expect("code description")
                .contains("JavaScript")
        );
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
