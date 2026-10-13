// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 zbrdc

//! Runs JavaScript in the sandboxed tardigrade V8 runtime.
//!
//! `Runner` resolves the tardigrade binary, fail-closes on
//! `tardigrade --sandbox-check`, and executes guest code under a
//! parent-enforced hard deadline plus a `prlimit --as` address-space cap.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Address-space belt (`prlimit --as`) in MiB: a floor, not the value
/// applied, because the runner raises it to cover V8's own reservation plus
/// the heap cap. It cannot make the guest's bound tighter than the heap cap.
pub const DEFAULT_AS_CAP_MB: u64 = 2048;

/// Heap and ArrayBuffer cap in MiB, passed to tardigrade as --memory.
///
/// The tardigrade default is 256. It is passed explicitly so the effective
/// cap appears in the result rather than being implied, and it is an operator
/// setting rather than a tool parameter: a model must not be able to lift its
/// own ceiling. Raise it with TARDIGRADE_RUNNER_MEM.
pub const DEFAULT_HEAP_MB: u64 = 256;

/// Tardigrade refuses --memory below this.
pub const MIN_HEAP_MB: u64 = 8;

/// Depth of guest recursion before V8 raises a catchable RangeError, measured
/// at roughly 10 500 frames. Not adjustable here: guest code deeper than this
/// must be made iterative.
pub const GUEST_RECURSION_LIMIT_HINT: u32 = 10_500;

/// Address space V8 reserves before any guest byte is allocated; the belt is
/// raised to at least this plus the heap cap.
///
/// Measured against tardigrade's current build (rusty_v8 139 with pointer
/// compression): a run works with 70 592 MiB of address space and aborts V8
/// ("Failed to reserve virtual memory for process-wide V8 pointer compression
/// cage") at 70 528 MiB, identical at `--memory=256` and `--memory=1024`, so
/// the reservation does not scale with the heap cap. 72 GiB leaves ~3 GiB of
/// slack. It is V8's reservation and not the guest's, which is why the belt
/// is a backstop here and the heap cap is the bound that binds.
const AS_FLOOR_MB: u64 = 72 * 1024;

/// Extra time past the guest's own `--timeout` before the parent SIGKILLs,
/// so tardigrade's clean exit 124 usually wins over a raw kill.
const KILL_GRACE_MS: u64 = 500;

/// A run must always be bounded from the parent side; one hour is plenty.
const MAX_TIMEOUT_MS: u32 = 3_600_000;

/// Per-stream capture cap while draining a running guest.
pub const DRAIN_CAP_BYTES: usize = 64 * 1024;

/// Largest end-of-run dump returned inline, in bytes (TARDIGRADE_RUNNER_DUMP_MAX).
pub const DEFAULT_DUMP_MAX_BYTES: u64 = 1024 * 1024;

/// RAM-filesystem quota in MiB, passed to tardigrade as --fs-quota.
/// Tardigrade's own default; passed explicitly and reported in the result,
/// and an operator setting (TARDIGRADE_RUNNER_FS_QUOTA) for the same reason
/// the heap cap is: a model must not lift its own ceilings. 0 disables it,
/// as tardigrade defines.
pub const DEFAULT_FS_QUOTA_MB: u64 = 64;

/// Largest guest script or preload snapshot accepted, in bytes
/// (TARDIGRADE_RUNNER_INPUT_MAX). Without it a caller could make the runner
/// write an arbitrarily large temp file; the outbound dump was always
/// capped, the inbound side was not.
pub const DEFAULT_INPUT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Per-response byte cap for granted network calls (tardigrade default).
pub const DEFAULT_NET_MAX_RESPONSE: u64 = 1024 * 1024;

/// Per-run network byte budget for granted calls (tardigrade default).
pub const DEFAULT_NET_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// Per-request network timeout in ms for granted calls (tardigrade default).
pub const DEFAULT_NET_TIMEOUT_MS: u64 = 30_000;

/// Appended to a stream whose drain hit [`DRAIN_CAP_BYTES`].
pub const TRUNCATED_MARKER: &str = "…[truncated]";

/// One operator-granted network endpoint: the guest may call
/// `vm.net.request(name, body)` and reach only `url`, by name. Grants are an
/// operator setting (TARDIGRADE_RUNNER_ALLOW_CALL), never a tool argument:
/// a model must not be able to grant itself egress. The URL is never shown
/// to the guest or returned in a result; only names are advertised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowCall {
    pub name: String,
    pub url: String,
    /// "GET", "POST" or "PUT"; None keeps tardigrade's POST default.
    pub method: Option<String>,
}

/// Parses the TARDIGRADE_RUNNER_ALLOW_CALL grammar: entries separated by
/// `;`, each `NAME=URL` or `NAME=URL,METHOD` (tardigrade's own flag shape,
/// so an operator can copy flags they already know).
pub fn parse_allow_calls(spec: &str) -> Result<Vec<AllowCall>, String> {
    let mut out = Vec::new();
    for entry in spec.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, rest) = entry
            .split_once('=')
            .ok_or_else(|| format!("bad allow-call {entry:?}: expected NAME=URL[,METHOD]"))?;
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "bad allow-call name {name:?}: names are ASCII letters, digits, '_' or '-'"
            ));
        }
        // The method, when present, follows the last comma; URLs may contain
        // commas of their own earlier in the string.
        let (url, method) = match rest.rsplit_once(',') {
            Some((url, m)) if ["GET", "POST", "PUT"].contains(&m.trim()) => {
                (url.trim(), Some(m.trim().to_owned()))
            }
            _ => (rest.trim(), None),
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!(
                "bad allow-call URL for {name:?}: expected http:// or https://"
            ));
        }
        if out.iter().any(|g: &AllowCall| g.name == name) {
            return Err(format!("duplicate allow-call name {name:?}"));
        }
        out.push(AllowCall {
            name: name.to_owned(),
            url: url.to_owned(),
            method,
        });
    }
    Ok(out)
}

#[derive(Debug)]
pub struct RunOptions {
    pub timeout_ms: u32,
    pub seed: u64,
    /// `"virtual"` passes tardigrade `--clock=virtual`, `"real"` omits it.
    pub clock: String,
    /// Start of the Date timeline under a virtual clock, in milliseconds since
    /// 1970. None keeps tardigrade's default of 0, under which Date.now() is 0
    /// and dates read as 1970: harmless for arithmetic, silently wrong for
    /// anything date-dependent, so it is reported back in the result.
    pub epoch_ms: Option<u64>,
    /// Snapshot JSON restored into the RAM filesystem before the guest starts
    /// (tardigrade --preload). Merge semantics: snapshot entries overwrite,
    /// entries the snapshot does not mention are kept.
    pub preload: Option<String>,
    /// Capture the RAM filesystem at the end of the run (tardigrade --dump)
    /// and return it as RunResult::dump, so a caller can hand it to a later
    /// run's preload.
    pub dump: bool,
    /// Arguments handed to the guest after the script path (tardigrade
    /// `[args...]`). Caller-controlled data, like the code itself.
    pub args: Vec<String>,
    /// Parse the entry as an ES module (tardigrade --module):
    /// import/export and top-level await, specifiers resolved inside the
    /// RAM filesystem.
    pub module: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            timeout_ms: 10_000,
            seed: 0,
            clock: "virtual".to_owned(),
            epoch_ms: None,
            preload: None,
            dump: false,
            args: Vec::new(),
            module: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Timeout,
    Error,
    /// The caller cancelled the run; the parent SIGKILLed the guest.
    Cancelled,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub status: RunStatus,
    /// The clock the guest ran on, and the base of its Date timeline, so a
    /// caller can tell a virtual 1970 from a real wall-clock run.
    pub clock: String,
    pub epoch_ms: Option<u64>,
    /// `None` when the process was killed by a signal (e.g. the parent SIGKILL).
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    /// Address-space cap in MiB actually applied; `None` only when prlimit
    /// is missing from PATH and `TARDIGRADE_ALLOW_UNCAPPED=1` allowed the
    /// uncapped run.
    pub memory_cap_mb: Option<u64>,
    /// Heap and ArrayBuffer cap in MiB passed to tardigrade.
    pub heap_limit_mb: u64,
    /// RAM-filesystem quota in MiB passed to tardigrade (0 = disabled).
    pub fs_quota_mb: u64,
    /// Names of the operator-granted network capabilities the guest could
    /// call with vm.net.request; empty when the guest was offline. Names
    /// only: URLs are operator policy and never leave the runner.
    pub net_capabilities: Vec<String>,
    /// End-of-run filesystem snapshot when dump was requested and the run
    /// wrote one; None otherwise (dump not requested, or the run ended before
    /// any state existed). Feed it back as preload to continue a workflow.
    pub dump: Option<String>,
}

/// Resolves the tardigrade binary: `TARDIGRADE_BIN` env, else the
/// `--tardigrade=<path>` flag, else `tardigrade` on PATH, else the default
/// checkout next to this one. Whichever wins still has to pass
/// `--sandbox-check` and the capability probe before anything runs.
pub fn resolve_bin(flag: Option<&str>) -> PathBuf {
    if let Some(bin) = std::env::var_os("TARDIGRADE_BIN") {
        return PathBuf::from(bin);
    }
    if let Some(bin) = flag {
        return PathBuf::from(bin);
    }
    let sibling = PathBuf::from("../tardigrade/target/release/tardigrade");
    if sibling.is_file() {
        return sibling;
    }
    if let Some(bin) = find_in_path("tardigrade") {
        return bin;
    }
    sibling
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Fail closed when prlimit is missing: an uncapped guest can exhaust host
/// memory, so only TARDIGRADE_ALLOW_UNCAPPED=1 explicitly accepts that risk.
fn uncapped_run_refusal(prlimit_missing: bool, allow_uncapped: bool) -> Option<&'static str> {
    (prlimit_missing && !allow_uncapped).then_some(
        "prlimit not found on PATH; refusing uncapped run (set TARDIGRADE_ALLOW_UNCAPPED=1 to override)",
    )
}

/// Flags the resolved tardigrade binary advertises in its usage line.
/// Tardigrade has no --version, so this probe is the compatibility
/// handshake: features the binary does not advertise are refused by name
/// instead of failing obscurely (or silently drifting) at spawn time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub serve: bool,
    pub allow_call: bool,
    pub fs_quota: bool,
    pub module: bool,
}

fn parse_capabilities(usage: &str) -> Capabilities {
    Capabilities {
        serve: usage.contains("--serve"),
        allow_call: usage.contains("--allow-call"),
        fs_quota: usage.contains("--fs-quota"),
        module: usage.contains("--module"),
    }
}

/// Probe the binary: the usage line covers most flags, but --module is
/// documented yet absent from it, so that one is probed directly — an
/// unknown flag is refused by name before the (missing) entry is opened.
fn probe_capabilities(bin: &Path) -> Capabilities {
    let usage = Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
        .unwrap_or_default();
    let mut caps = parse_capabilities(&usage);
    if !caps.module {
        caps.module = Command::new(bin)
            .args(["--module", "/nonexistent-tardigrade-runner-probe.js"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stderr).contains("unknown flag"))
            .unwrap_or(false);
    }
    caps
}

/// One operator setting from the environment: absent keeps the default,
/// present-but-unparsable is a startup error that names the variable.
fn env_setting<T: std::str::FromStr>(name: &str, default: T, unit: &str) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(v) => v
            .parse::<T>()
            .map_err(|e| format!("bad {name} {v:?}: {e} (expected {unit})")),
        Err(_) => Ok(default),
    }
}

pub struct Runner {
    bin: PathBuf,
    /// Only set by a successful `--sandbox-check`; every other state refuses runs.
    sandbox_ok: bool,
    /// Probed alongside the sandbox check; all-false until then.
    caps: Capabilities,
    prlimit: Option<PathBuf>,
    as_cap_mb: u64,
    heap_mb: u64,
    /// Largest dump returned inline; oversize dumps are suppressed with a
    /// stderr note rather than ballooning an MCP message.
    dump_max_bytes: u64,
    fs_quota_mb: u64,
    input_max_bytes: usize,
    /// Operator-granted network endpoints; empty means guests are offline
    /// and no net flags are passed at all.
    allow_calls: Vec<AllowCall>,
    net_max_response: u64,
    net_total_bytes: u64,
    net_timeout_ms: u64,
    /// Operator opt-in only (TARDIGRADE_RUNNER_NET_ALLOW_PRIVATE=1):
    /// granted endpoints may resolve to private/loopback addresses.
    net_allow_private: bool,
}

impl Runner {
    pub fn new(bin: PathBuf) -> Result<Runner, String> {
        let allow_calls = match std::env::var("TARDIGRADE_RUNNER_ALLOW_CALL") {
            Ok(v) => parse_allow_calls(&v)
                .map_err(|e| format!("bad TARDIGRADE_RUNNER_ALLOW_CALL: {e}"))?,
            Err(_) => Vec::new(),
        };
        Ok(Runner {
            bin,
            sandbox_ok: false,
            caps: Capabilities::default(),
            prlimit: find_in_path("prlimit"),
            as_cap_mb: env_setting("TARDIGRADE_RUNNER_AS", DEFAULT_AS_CAP_MB, "MiB")?,
            heap_mb: env_setting("TARDIGRADE_RUNNER_MEM", DEFAULT_HEAP_MB, "MiB")?,
            dump_max_bytes: env_setting(
                "TARDIGRADE_RUNNER_DUMP_MAX",
                DEFAULT_DUMP_MAX_BYTES,
                "bytes",
            )?,
            fs_quota_mb: env_setting("TARDIGRADE_RUNNER_FS_QUOTA", DEFAULT_FS_QUOTA_MB, "MiB")?,
            input_max_bytes: env_setting(
                "TARDIGRADE_RUNNER_INPUT_MAX",
                DEFAULT_INPUT_MAX_BYTES,
                "bytes",
            )?,
            allow_calls,
            net_max_response: env_setting(
                "TARDIGRADE_RUNNER_NET_MAX_RESPONSE",
                DEFAULT_NET_MAX_RESPONSE,
                "bytes",
            )?,
            net_total_bytes: env_setting(
                "TARDIGRADE_RUNNER_NET_TOTAL_BYTES",
                DEFAULT_NET_TOTAL_BYTES,
                "bytes",
            )?,
            net_timeout_ms: env_setting(
                "TARDIGRADE_RUNNER_NET_TIMEOUT_MS",
                DEFAULT_NET_TIMEOUT_MS,
                "ms",
            )?,
            net_allow_private: std::env::var("TARDIGRADE_RUNNER_NET_ALLOW_PRIVATE").as_deref()
                == Ok("1"),
        })
    }

    /// Set the dump cap in bytes. For operators and tests.
    pub fn with_dump_max(mut self, bytes: u64) -> Runner {
        self.dump_max_bytes = bytes;
        self
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// Heap and ArrayBuffer cap in MiB this runner will apply.
    pub fn heap_mb(&self) -> u64 {
        self.heap_mb
    }

    /// Set the heap cap. For operators and tests; the server reads
    /// TARDIGRADE_RUNNER_MEM instead.
    pub fn with_heap_mb(mut self, mb: u64) -> Runner {
        self.heap_mb = mb;
        self
    }

    /// RAM-filesystem quota in MiB this runner will apply.
    pub fn fs_quota_mb(&self) -> u64 {
        self.fs_quota_mb
    }

    /// Set the RAM-filesystem quota. For operators and tests.
    pub fn with_fs_quota_mb(mut self, mb: u64) -> Runner {
        self.fs_quota_mb = mb;
        self
    }

    /// Set the inbound code/preload size cap. For operators and tests.
    pub fn with_input_max(mut self, bytes: usize) -> Runner {
        self.input_max_bytes = bytes;
        self
    }

    /// Grant network endpoints. For operators and tests; the server reads
    /// TARDIGRADE_RUNNER_ALLOW_CALL instead. Grants take effect only if
    /// the probed binary supports --allow-call (checked at the gate).
    pub fn with_allow_calls(mut self, grants: Vec<AllowCall>) -> Runner {
        self.allow_calls = grants;
        self
    }

    /// Allow granted endpoints on private/loopback addresses. For
    /// operators and tests; off by default because that is where SSRF
    /// lives.
    pub fn with_net_allow_private(mut self, allow: bool) -> Runner {
        self.net_allow_private = allow;
        self
    }

    /// Names of the granted network capabilities, for tool descriptions
    /// and results. URLs are never exposed through this API.
    pub fn net_capability_names(&self) -> Vec<String> {
        self.allow_calls.iter().map(|g| g.name.clone()).collect()
    }

    /// Whether granted endpoints may use private addresses (operator only).
    pub fn net_allow_private(&self) -> bool {
        self.net_allow_private
    }

    /// Capabilities probed from the binary at the last sandbox check.
    pub fn capabilities(&self) -> Capabilities {
        self.caps
    }

    /// The address-space belt actually applied: the configured cap, raised
    /// when needed to cover the V8 reservations plus the heap cap.
    fn effective_as_mb(&self) -> u64 {
        self.as_cap_mb.max(self.heap_mb + AS_FLOOR_MB)
    }

    /// Fail-closed gate: `run_js` refuses everything unless this returned `Ok`.
    pub fn sandbox_check(&mut self) -> Result<(), String> {
        self.sandbox_ok = false;
        let out = Command::new(&self.bin)
            .arg("--sandbox-check")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.bin.display()))?;
        if out.status.success() {
            self.caps = probe_capabilities(&self.bin);
            // Fail closed on a binary too old for the flags this runner
            // passes unconditionally, or for configured grants: a named
            // refusal now beats "unknown flag" (or silent drift) later.
            if !self.caps.fs_quota {
                return Err(format!(
                    "{} does not advertise --fs-quota; refusing to run against an incompatible tardigrade",
                    self.bin.display()
                ));
            }
            if !self.allow_calls.is_empty() && !self.caps.allow_call {
                return Err(format!(
                    "{} does not advertise --allow-call but network grants are configured; refusing",
                    self.bin.display()
                ));
            }
            self.sandbox_ok = true;
            return Ok(());
        }
        Err(format!(
            "{} --sandbox-check failed ({}): {}",
            self.bin.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }

    /// Everything that can be refused before a process exists. Input size
    /// is checked first: it is the cheapest refusal and the one that keeps
    /// an oversize temp file from ever being written.
    fn validate_run(&self, code: &str, opts: &RunOptions) -> Result<(), String> {
        if code.len() > self.input_max_bytes {
            return Err(format!(
                "code is {} bytes, over the {} byte input cap (raise TARDIGRADE_RUNNER_INPUT_MAX)",
                code.len(),
                self.input_max_bytes
            ));
        }
        if let Some(preload) = &opts.preload {
            if preload.len() > self.input_max_bytes {
                return Err(format!(
                    "preload is {} bytes, over the {} byte input cap (raise TARDIGRADE_RUNNER_INPUT_MAX)",
                    preload.len(),
                    self.input_max_bytes
                ));
            }
        }
        if opts.module && !self.caps.module {
            return Err(
                "this tardigrade binary does not support --module; refusing a module run".into(),
            );
        }
        if opts.timeout_ms == 0 || opts.timeout_ms > MAX_TIMEOUT_MS {
            return Err(format!("timeout_ms must be in 1..={MAX_TIMEOUT_MS}"));
        }
        if self.heap_mb < MIN_HEAP_MB {
            return Err(format!("heap_mb must be at least {MIN_HEAP_MB}"));
        }
        if opts.clock != "virtual" && opts.clock != "real" {
            return Err(r#"clock must be "virtual" or "real""#.into());
        }
        if let Some(epoch) = opts.epoch_ms {
            if opts.clock != "virtual" {
                return Err("epoch_ms needs the virtual clock; pass clock = \"virtual\"".into());
            }
            if epoch > 8_640_000_000_000_000 {
                return Err("epoch_ms is past the ECMAScript maximum time value".into());
            }
        }
        if let Some(preload) = &opts.preload {
            let is_snapshot = serde_json::from_str::<serde_json::Value>(preload)
                .map(|v| v.is_object())
                .unwrap_or(false);
            if !is_snapshot {
                return Err(
                    "preload must be a snapshot JSON object (the dump field of an earlier run qualifies)"
                        .into(),
                );
            }
        }
        Ok(())
    }

    pub fn run_js(&self, code: &str, opts: &RunOptions) -> Result<RunResult, String> {
        self.run_js_cancellable(code, opts, None)
    }

    /// Like `run_js`, but setting `cancel` SIGKILLs the guest promptly and
    /// reports `RunStatus::Cancelled` — the parent wait loop already polls
    /// every few milliseconds, so cancellation costs no extra machinery.
    pub fn run_js_cancellable(
        &self,
        code: &str,
        opts: &RunOptions,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<RunResult, String> {
        self.validate_run(code, opts)?;
        if !self.sandbox_ok {
            return Err("tardigrade --sandbox-check has not passed; refusing to run code".into());
        }
        let allow_uncapped = std::env::var("TARDIGRADE_ALLOW_UNCAPPED").as_deref() == Ok("1");
        if let Some(msg) = uncapped_run_refusal(self.prlimit.is_none(), allow_uncapped) {
            return Err(msg.to_owned());
        }
        let script = write_temp_script(code)?;
        let _script_guard = TempScript(&script);
        // Both files live in the same 0700 process-private directory as the
        // script: under --sandbox the dump inode is the only file the guest
        // can write, so it must pre-exist as a regular file we own.
        let preload_path = match opts.preload.as_deref() {
            Some(json) => Some(write_temp_file(temp_name("preload", "json"), json)?),
            None => None,
        };
        let _preload_guard = preload_path.as_deref().map(TempScript);
        let dump_path = if opts.dump {
            Some(write_temp_file(temp_name("dump", "json"), "")?)
        } else {
            None
        };
        let _dump_guard = dump_path.as_deref().map(TempScript);
        let mut result = self.spawn_and_wait(
            &script,
            preload_path.as_deref(),
            dump_path.as_deref(),
            opts,
            cancel,
        )?;
        if let Some(dump_file) = dump_path.as_deref() {
            let (dump, note) = self.read_dump(dump_file);
            if let Some(note) = note {
                result.stderr = format!("{}{note}", result.stderr);
            }
            result.dump = dump;
        }
        Ok(result)
    }

    /// Read a finished dump file under the inline cap; oversize snapshots
    /// are suppressed with a note rather than ballooning a reply.
    fn read_dump(&self, dump_file: &Path) -> (Option<String>, Option<String>) {
        let len = fs::metadata(dump_file).map(|m| m.len()).unwrap_or(0);
        if len > self.dump_max_bytes {
            return (
                None,
                Some(format!(
                    "tardigrade-runner: dump suppressed ({len} bytes > {} cap); keep handed-off state small or raise TARDIGRADE_RUNNER_DUMP_MAX\n",
                    self.dump_max_bytes
                )),
            );
        }
        (
            fs::read_to_string(dump_file)
                .ok()
                .filter(|content| !content.is_empty()),
            None,
        )
    }

    fn spawn_and_wait(
        &self,
        script: &Path,
        preload: Option<&Path>,
        dump: Option<&Path>,
        opts: &RunOptions,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<RunResult, String> {
        let start = Instant::now();
        let mut child = self
            .build_command(script, preload, dump, opts)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot spawn tardigrade: {e}"))?;

        // Drain while the child runs: a guest that out-prints the kernel pipe
        // buffer would otherwise block on write, defeat its own watchdog, and
        // get SIGKILLed as a "timeout" with all output lost.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_thread = std::thread::spawn(move || drain_pipe(stdout_pipe));
        let stderr_thread = std::thread::spawn(move || drain_pipe(stderr_pipe));

        // The guest's own --timeout (same value) fires first; the parent only
        // SIGKILLs after a grace period, for the cases the guest watchdog
        // cannot stop (a stuck thread).
        let deadline = Duration::from_millis(u64::from(opts.timeout_ms) + KILL_GRACE_MS);
        let (status, kill) = wait_with_deadline(&mut child, deadline, cancel)?;
        let stdout = stdout_thread
            .join()
            .map_err(|_| "stdout drain thread panicked".to_owned())?;
        let stderr = stderr_thread
            .join()
            .map_err(|_| "stderr drain thread panicked".to_owned())?;
        let stderr = with_abort_note(stderr, self.heap_mb);
        let (status_kind, exit_code) = classify(status, kill);

        Ok(RunResult {
            status: status_kind,
            clock: opts.clock.clone(),
            epoch_ms: opts.epoch_ms,
            exit_code,
            stdout,
            stderr,
            duration_ms: start.elapsed().as_millis(),
            memory_cap_mb: self.prlimit.is_some().then_some(self.effective_as_mb()),
            heap_limit_mb: self.heap_mb,
            fs_quota_mb: self.fs_quota_mb,
            net_capabilities: self.net_capability_names(),
            dump: None,
        })
    }

    /// Flags shared by one-shot runs and sessions: the sandbox, the caps,
    /// the clock, and any operator-granted network endpoints. Net flags
    /// are passed only when grants exist — with none, the guest is fully
    /// offline and vm.net.request does not exist, exactly as before.
    fn common_guest_args(
        &self,
        preload: Option<&Path>,
        dump: Option<&Path>,
        opts: &RunOptions,
    ) -> Vec<String> {
        let mut guest_args: Vec<String> = vec!["--sandbox".to_owned()];
        if let Some(preload) = preload {
            guest_args.push(format!("--preload={}", preload.display()));
        }
        if let Some(dump) = dump {
            guest_args.push(format!("--dump={}", dump.display()));
        }
        if opts.clock == "virtual" {
            guest_args.push("--clock=virtual".to_owned());
        }
        guest_args.push(format!("--seed={}", opts.seed));
        if let Some(epoch) = opts.epoch_ms {
            guest_args.push(format!("--epoch={epoch}"));
        }
        guest_args.push(format!("--memory={}", self.heap_mb));
        guest_args.push(format!("--fs-quota={}", self.fs_quota_mb));
        if opts.module {
            guest_args.push("--module".to_owned());
        }
        if !self.allow_calls.is_empty() {
            for grant in &self.allow_calls {
                let mut spec = format!("--allow-call={}={}", grant.name, grant.url);
                if let Some(method) = &grant.method {
                    spec.push(',');
                    spec.push_str(method);
                }
                guest_args.push(spec);
            }
            guest_args.push(format!("--net-max-response={}", self.net_max_response));
            guest_args.push(format!("--net-total-bytes={}", self.net_total_bytes));
            guest_args.push(format!("--net-timeout={}", self.net_timeout_ms));
            if self.net_allow_private {
                guest_args.push("--net-allow-private".to_owned());
            }
        }
        guest_args
    }

    /// Wrap guest args in the prlimit address-space belt when available.
    fn wrap_command(&self, guest_args: &[String]) -> Command {
        match &self.prlimit {
            Some(prlimit) => {
                let mut cmd = Command::new(prlimit);
                // `--` keeps prlimit from parsing tardigrade's own flags.
                cmd.arg(format!("--as={}", self.effective_as_mb() * 1024 * 1024));
                cmd.arg("--").arg(&self.bin).args(guest_args);
                cmd
            }
            None => {
                let mut cmd = Command::new(&self.bin);
                cmd.args(guest_args);
                cmd
            }
        }
    }

    fn build_command(
        &self,
        script: &Path,
        preload: Option<&Path>,
        dump: Option<&Path>,
        opts: &RunOptions,
    ) -> Command {
        let mut guest_args = self.common_guest_args(preload, dump, opts);
        guest_args.push(format!("--timeout={}", opts.timeout_ms));
        guest_args.push(script.to_string_lossy().into_owned());
        guest_args.extend(opts.args.iter().cloned());
        self.wrap_command(&guest_args)
    }

    /// Open a persistent `--serve` session: one guest process whose
    /// globals, timers and RAM files survive across `Session::eval`
    /// calls. `opts.timeout_ms` is the per-command timeout (mandatory:
    /// without it leftover guest work can block settling forever) and
    /// `ttl_ms` bounds the whole session as tardigrade's --timeout.
    /// The same fail-closed gate, caps and input limits as `run_js` apply.
    pub fn open_session(
        &self,
        entry_code: Option<&str>,
        opts: &RunOptions,
        ttl_ms: u32,
    ) -> Result<Session, String> {
        if let Some(code) = entry_code {
            self.validate_run(code, opts)?;
        } else {
            self.validate_run("", opts)?;
        }
        if !self.sandbox_ok {
            return Err(
                "tardigrade --sandbox-check has not passed; refusing to open a session".into(),
            );
        }
        if !self.caps.serve {
            return Err(
                "this tardigrade binary does not advertise --serve; sessions are unavailable"
                    .into(),
            );
        }
        if ttl_ms == 0 || ttl_ms > MAX_TIMEOUT_MS {
            return Err(format!("ttl_ms must be in 1..={MAX_TIMEOUT_MS}"));
        }
        let allow_uncapped = std::env::var("TARDIGRADE_ALLOW_UNCAPPED").as_deref() == Ok("1");
        if let Some(msg) = uncapped_run_refusal(self.prlimit.is_none(), allow_uncapped) {
            return Err(msg.to_owned());
        }

        // No TempScript guards here: the guest opens these files after
        // spawn and may read the entry lazily, so the Session owns the
        // paths and removes them on close/drop.
        let entry_path = match entry_code {
            Some(code) => Some(write_temp_script(code)?),
            None => None,
        };
        let preload_path = match opts.preload.as_deref() {
            Some(json) => Some(write_temp_file(temp_name("preload", "json"), json)?),
            None => None,
        };
        let dump_path = if opts.dump {
            Some(write_temp_file(temp_name("dump", "json"), "")?)
        } else {
            None
        };

        let mut guest_args =
            self.common_guest_args(preload_path.as_deref(), dump_path.as_deref(), opts);
        guest_args.push("--serve".to_owned());
        guest_args.push(format!("--command-timeout={}", opts.timeout_ms));
        guest_args.push(format!("--timeout={ttl_ms}"));
        if let Some(entry) = &entry_path {
            guest_args.push(entry.to_string_lossy().into_owned());
            guest_args.extend(opts.args.iter().cloned());
        }

        let mut child = self
            .wrap_command(&guest_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                for path in [&entry_path, &preload_path, &dump_path]
                    .into_iter()
                    .flatten()
                {
                    let _ = fs::remove_file(path);
                }
                format!("cannot spawn tardigrade session: {e}")
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "session stdin unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "session stdout unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "session stderr unavailable".to_owned())?;

        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        {
            let stderr_buf = std::sync::Arc::clone(&stderr_buf);
            std::thread::spawn(move || drain_into(stderr, stderr_buf));
        }

        Ok(Session {
            child,
            stdin: Some(stdin),
            replies: rx,
            stderr_buf,
            command_timeout_ms: opts.timeout_ms,
            input_max_bytes: self.input_max_bytes,
            heap_mb: self.heap_mb,
            dump_path,
            dump_max_bytes: self.dump_max_bytes,
            _entry_guard: entry_path,
            _preload_guard: preload_path,
            alive: true,
        })
    }
}

/// Why the parent ended a run, if it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kill {
    None,
    Deadline,
    Cancelled,
}

/// One `--serve` command's outcome: the raw JSON reply line from the guest
/// (`{"$error":...}` included — a guest error is a reply, not a transport
/// failure) plus whatever the guest printed to stderr since the last call.
#[derive(Debug)]
pub struct SessionReply {
    pub reply: String,
    pub stderr: String,
    pub duration_ms: u128,
}

/// What a closed session leaves behind.
#[derive(Debug)]
pub struct SessionClose {
    pub exit_code: Option<i32>,
    pub stderr: String,
    /// End-of-session filesystem snapshot when dump was requested.
    pub dump: Option<String>,
}

/// A live tardigrade `--serve` process. One expression per `eval`, one
/// JSON reply line back; guest console output arrives on stderr and is
/// returned (capped) with each reply. Dropping a session kills it.
pub struct Session {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    replies: std::sync::mpsc::Receiver<String>,
    stderr_buf: std::sync::Arc<std::sync::Mutex<String>>,
    command_timeout_ms: u32,
    input_max_bytes: usize,
    heap_mb: u64,
    dump_path: Option<PathBuf>,
    dump_max_bytes: u64,
    // Entry/preload temp files the guest may still open lazily; removed
    // on close/drop rather than at spawn.
    _entry_guard: Option<PathBuf>,
    _preload_guard: Option<PathBuf>,
    alive: bool,
}

impl Session {
    pub fn is_alive(&mut self) -> bool {
        if self.alive {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => self.alive = false,
                Ok(None) => {}
            }
        }
        self.alive
    }

    /// Evaluate one expression. The serve protocol is one line per
    /// command, so multi-line input is refused here rather than being
    /// silently split into commands the caller did not intend.
    pub fn eval(&mut self, expr: &str) -> Result<SessionReply, String> {
        if !self.is_alive() {
            return Err("session has ended".into());
        }
        if expr.len() > self.input_max_bytes {
            return Err(format!(
                "expression is {} bytes, over the {} byte input cap",
                expr.len(),
                self.input_max_bytes
            ));
        }
        if expr.contains('\n') || expr.contains('\r') {
            return Err(
                "a session command is one line (one expression); wrap statements in an IIFE, e.g. (() => { ... })()"
                    .into(),
            );
        }
        let start = Instant::now();
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "session has ended".to_owned())?;
        writeln!(stdin, "{expr}")
            .and_then(|()| stdin.flush())
            .map_err(|e| {
                self.alive = false;
                format!("cannot write to session: {e}")
            })?;
        // The guest answers within its command timeout, but settling the
        // previous command's leftovers can consume another full budget
        // before this reply — allow two budgets plus the kill grace
        // before declaring the session wedged and killing it.
        let wait =
            Duration::from_millis(u64::from(self.command_timeout_ms) * 2 + KILL_GRACE_MS + 1_000);
        match self.replies.recv_timeout(wait) {
            Ok(reply) => Ok(SessionReply {
                reply,
                stderr: with_abort_note(self.take_stderr(), self.heap_mb),
                duration_ms: start.elapsed().as_millis(),
            }),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.alive = false;
                Err(format!(
                    "session did not reply within {} ms and was killed",
                    wait.as_millis()
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.alive = false;
                let stderr = with_abort_note(self.take_stderr(), self.heap_mb);
                Err(format!("session ended before replying; stderr: {stderr}"))
            }
        }
    }

    fn take_stderr(&self) -> String {
        let mut buf = self
            .stderr_buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *buf)
    }

    /// Close the command channel and reap the guest, collecting the
    /// end-of-session dump when one was requested.
    pub fn close(mut self) -> SessionClose {
        drop(self.stdin.take());
        let (status, _) = wait_with_deadline(&mut self.child, Duration::from_secs(2), None)
            .unwrap_or_else(|_| {
                let _ = self.child.kill();
                (
                    self.child.wait().expect("reap killed session"),
                    Kill::Deadline,
                )
            });
        self.alive = false;
        // Give the stderr drain a moment to deliver the final bytes.
        std::thread::sleep(Duration::from_millis(10));
        let mut stderr = with_abort_note(self.take_stderr(), self.heap_mb);
        let dump = self.dump_path.as_deref().and_then(|dump_file| {
            let len = fs::metadata(dump_file).map(|m| m.len()).unwrap_or(0);
            if len > self.dump_max_bytes {
                stderr.push_str(&format!(
                    "tardigrade-runner: dump suppressed ({len} bytes > {} cap); keep handed-off state small or raise TARDIGRADE_RUNNER_DUMP_MAX\n",
                    self.dump_max_bytes
                ));
                None
            } else {
                fs::read_to_string(dump_file)
                    .ok()
                    .filter(|content| !content.is_empty())
            }
        });
        SessionClose {
            exit_code: status.code(),
            stderr,
            dump,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.alive {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        for path in [&self._entry_guard, &self._preload_guard, &self.dump_path]
            .into_iter()
            .flatten()
        {
            let _ = fs::remove_file(path);
        }
    }
}

/// Continuously drain a session's stderr into a shared buffer, keeping
/// the most recent DRAIN_CAP_BYTES so a chatty guest cannot grow it (or
/// block on a full pipe) over a long session.
fn drain_into(mut pipe: impl Read, buf: std::sync::Arc<std::sync::Mutex<String>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&chunk[..n]);
                let mut guard = buf
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.push_str(&text);
                if guard.len() > DRAIN_CAP_BYTES {
                    let mut cut = guard.len() - DRAIN_CAP_BYTES;
                    while cut < guard.len() && !guard.is_char_boundary(cut) {
                        cut += 1;
                    }
                    let tail = guard[cut..].to_owned();
                    *guard = format!("{TRUNCATED_MARKER}{tail}");
                }
            }
        }
    }
}

static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Remove this process's script directory, best effort.
///
/// TempScript removes each script file, but nothing removed the directory
/// holding them, so a server lifetime used to leave an empty
/// tardigrade-runner-<pid> behind in /tmp. Call this once, when the process is
/// finished with the runner: the directory is shared by every [`Runner`] in the
/// process, so tying the cleanup to a single Runner would let one instance
/// delete the directory another is still writing into.
pub fn cleanup_script_dir() {
    let _ = fs::remove_dir_all(script_dir_path());
}

/// Best-effort cleanup: Drop removes the guest script even on panic or an
/// early return between writing it and the run finishing.
struct TempScript<'a>(&'a Path);

impl Drop for TempScript<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

/// Where this process keeps the guest scripts it writes.
///
/// Shared by the writer and by the cleanup in Drop for Runner, so the two
/// cannot drift apart.
fn script_dir_path() -> PathBuf {
    std::env::temp_dir().join(format!("tardigrade-runner-{}", std::process::id()))
}

/// 0700 + atomic mkdir closes the /tmp symlink hole: a private dir only we
/// can enter means a pre-planted symlink can never redirect our writes.
/// chmod also proves we own a pre-existing dir (mkdir returned EEXIST).
fn script_dir() -> Result<PathBuf, String> {
    let dir = script_dir_path();
    match fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(&dir)
                .map_err(|e| format!("cannot inspect {}: {e}", dir.display()))?;
            if !meta.is_dir() {
                return Err(format!("{} exists but is not a directory", dir.display()));
            }
        }
        Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot chmod {} to 0700: {e} (not ours?)", dir.display()))?;
    }
    Ok(dir)
}

/// Unique temp file name: the script directory is shared by concurrent runs
/// in this process, so every file it hands out needs its own sequence number.
fn temp_name(prefix: &str, ext: &str) -> String {
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{seq}.{ext}")
}
fn write_temp_script(code: &str) -> Result<PathBuf, String> {
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::Relaxed);
    write_temp_file(format!("{seq}.js"), code)
}

fn write_temp_file(name: String, contents: &str) -> Result<PathBuf, String> {
    let path = script_dir()?.join(name);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 0600: guest code may embed secrets; only this uid may read it.
        opts.mode(0o600);
    }
    let mut file = opts
        .open(&path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn wait_with_deadline(
    child: &mut Child,
    deadline: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(ExitStatus, Kill), String> {
    let end = Instant::now() + deadline;
    loop {
        let status = child
            .try_wait()
            .map_err(|e| format!("waiting for tardigrade failed: {e}"))?;
        if let Some(status) = status {
            return Ok((status, Kill::None));
        }
        let cancelled = cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        if cancelled || Instant::now() >= end {
            let _ = child.kill();
            let status = child
                .wait()
                .map_err(|e| format!("reaping killed tardigrade failed: {e}"))?;
            return Ok((
                status,
                if cancelled {
                    Kill::Cancelled
                } else {
                    Kill::Deadline
                },
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Explain the guest failures a caller can act on, leaving the original
/// stderr intact above the note. `heap_mb` is the cap actually applied —
/// an earlier version of this note hardcoded the 256 MiB default and
/// misreported raised caps. Current tardigrade stubs Intl to a catchable
/// error (older builds aborted inside ICU); both shapes get the note.
fn with_abort_note(stderr: String, heap_mb: u64) -> String {
    let note = if stderr.contains("Intl is not available in this build")
        || stderr.contains("DateTimePatternGeneratorCache")
        || stderr.contains("Icu error")
    {
        "tardigrade-runner: this build ships no ICU data, so Intl and the toLocale* methods are unavailable (a known tardigrade limitation, not your script). Format dates and numbers by hand, and pass clock: real if the current time is needed.".to_owned()
    } else if stderr.contains("Fatal process out of memory") {
        format!(
            "tardigrade-runner: the guest hit a hard memory limit and V8 aborted it. The heap and ArrayBuffer were each capped at {heap_mb} MiB for this run; exceeding them normally raises a catchable RangeError, so an abort here means V8 ran out of room before guest code could catch the limit."
        )
    } else {
        return stderr;
    };
    format!("{stderr}\n{note}")
}

fn classify(status: ExitStatus, kill: Kill) -> (RunStatus, Option<i32>) {
    match kill {
        Kill::Cancelled => return (RunStatus::Cancelled, None),
        Kill::Deadline => return (RunStatus::Timeout, None),
        Kill::None => {}
    }
    match status.code() {
        Some(0) => (RunStatus::Ok, Some(0)),
        Some(124) => (RunStatus::Timeout, Some(124)),
        Some(code) => (RunStatus::Error, Some(code)),
        None => (RunStatus::Error, None),
    }
}

fn drain_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buf = Vec::with_capacity(DRAIN_CAP_BYTES);
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.len() >= DRAIN_CAP_BYTES {
            // Cap hit: returning drops the pipe (closing it), so a chatty
            // guest's next write fails instead of blocking forever on a full
            // kernel pipe buffer.
            buf.truncate(DRAIN_CAP_BYTES);
            return String::from_utf8_lossy(&buf).into_owned() + TRUNCATED_MARKER;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncapped_runs_refused_only_without_override() {
        let refused = uncapped_run_refusal(true, false).expect("missing prlimit must refuse");
        assert!(refused.contains("TARDIGRADE_ALLOW_UNCAPPED=1"));
        assert_eq!(uncapped_run_refusal(true, true), None);
        assert_eq!(uncapped_run_refusal(false, false), None);
    }

    #[test]
    fn temp_script_lives_in_private_dir_with_tight_modes() {
        let script = write_temp_script("console.log(1);").expect("write temp script");
        let dir = std::env::temp_dir().join(format!("tardigrade-runner-{}", std::process::id()));
        assert!(
            script.starts_with(&dir),
            "script {:?} not inside {dir:?}",
            script
        );
        let dir_meta = fs::metadata(&dir).expect("dir metadata");
        assert!(dir_meta.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);
            assert_eq!(
                fs::metadata(&script)
                    .expect("script mode")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_to_string(&script).unwrap(), "console.log(1);");
        drop(TempScript(&script));
        assert!(!script.exists(), "guard must remove the script");
        assert!(
            dir.exists(),
            "per-process dir stays for reuse within the process"
        );

        // Cleanup is explicit and process-scoped, not tied to one Runner's Drop:
        // the directory is shared by every Runner in the process. Both tests
        // below share this directory, so they live in one test rather than
        // racing each other.
        cleanup_script_dir();
        assert!(!dir.exists(), "cleanup must remove {dir:?}");
        cleanup_script_dir(); // and must be harmless when it is already gone
    }

    #[test]
    fn drain_pipe_caps_at_limit_and_marks() {
        use std::io::Cursor;
        let long = drain_pipe(Some(Cursor::new(vec![b'x'; DRAIN_CAP_BYTES + 4096])));
        assert_eq!(long.len(), DRAIN_CAP_BYTES + TRUNCATED_MARKER.len());
        assert!(long.ends_with(TRUNCATED_MARKER));

        let short = drain_pipe(Some(Cursor::new(b"hi".to_vec())));
        assert_eq!(short, "hi");
        assert_eq!(drain_pipe(None::<std::io::Empty>), "");
    }

    #[test]
    fn allow_calls_parse_names_urls_and_methods_only() {
        let grants = parse_allow_calls(
            "ingest=https://api.example.com/ingest; catalog=https://api.example.com/c?q=1,2,GET",
        )
        .expect("valid grants");
        assert_eq!(grants.len(), 2);
        assert_eq!(grants[0].name, "ingest");
        assert_eq!(grants[0].method, None);
        // A comma inside the URL is not a method separator.
        assert_eq!(grants[1].url, "https://api.example.com/c?q=1,2");
        assert_eq!(grants[1].method.as_deref(), Some("GET"));

        assert!(
            parse_allow_calls("x=ftp://host/").is_err(),
            "non-http refused"
        );
        assert!(parse_allow_calls("https://host/").is_err(), "name required");
        assert!(
            parse_allow_calls("a=https://h/;a=https://h/2").is_err(),
            "duplicate names refused"
        );
        assert!(parse_allow_calls("").expect("empty is fine").is_empty());
    }

    #[test]
    fn capabilities_come_from_the_usage_line() {
        let caps = parse_capabilities(
            "tardigrade [--serve] [--fs-quota=<mb>] [--allow-call=NAME=URL] <entry.js>",
        );
        assert!(caps.serve && caps.fs_quota && caps.allow_call);
        assert!(!caps.module, "--module is probed separately");
        assert_eq!(parse_capabilities(""), Capabilities::default());
    }

    #[test]
    fn abort_notes_name_the_cap_actually_applied() {
        let noted = with_abort_note("Fatal process out of memory".into(), 512);
        assert!(noted.contains("512 MiB"), "note was {noted:?}");
        assert!(!noted.contains("256 MiB"), "must not hardcode the default");

        let intl = with_abort_note(
            "tardigrade: uncaught exception: Intl is not available in this build (no ICU data)"
                .into(),
            256,
        );
        assert!(intl.contains("no ICU data"), "note was {intl:?}");
        // Ordinary errors pass through untouched.
        assert_eq!(
            with_abort_note("SyntaxError: x".into(), 256),
            "SyntaxError: x"
        );
    }

    #[test]
    fn oversize_inputs_are_refused_before_any_spawn() {
        // No sandbox check on this runner: input validation must fire
        // first, so the refusal proves no temp file needed writing.
        let runner = Runner::new(PathBuf::from("unused"))
            .expect("runner config")
            .with_input_max(8);
        let err = runner
            .run_js("console.log('way too long')", &RunOptions::default())
            .expect_err("oversize code must refuse");
        assert!(err.contains("input cap"), "err was {err:?}");

        let opts = RunOptions {
            preload: Some("{\"padding\":\"also way too long\"}".into()),
            ..RunOptions::default()
        };
        let err = runner
            .run_js("1", &opts)
            .expect_err("oversize preload must refuse");
        assert!(err.contains("preload"), "err was {err:?}");
    }
}
