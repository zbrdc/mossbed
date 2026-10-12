//! Runs JavaScript in the sandboxed tardigrade V8 runtime.
//!
//! `Runner` resolves the tardigrade binary, fail-closes on
//! `tardigrade --sandbox-check`, and executes guest code under a
//! parent-enforced hard deadline plus a `prlimit --as` address-space cap.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Address-space cap (MiB) applied with `prlimit --as`.
///
/// V8 aborts at startup below ~1.3 GiB of address space because it reserves
/// the pointer-compression cage and the code range up front (verified against
/// the real binary: `--as=512MiB` dies with "Check failed: Start()").
/// 2 GiB lets V8 start while still bounding a runaway allocator.
pub const DEFAULT_AS_CAP_MB: u64 = 2048;

/// Extra time past the guest's own `--timeout` before the parent SIGKILLs,
/// so tardigrade's clean exit 124 usually wins over a raw kill.
const KILL_GRACE_MS: u64 = 500;

/// A run must always be bounded from the parent side; one hour is plenty.
const MAX_TIMEOUT_MS: u32 = 3_600_000;

#[derive(Debug)]
pub struct RunOptions {
    pub timeout_ms: u32,
    pub seed: u32,
    /// `"virtual"` passes tardigrade `--clock=virtual`, `"real"` omits it.
    pub clock: String,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            timeout_ms: 10_000,
            seed: 0,
            clock: "virtual".to_owned(),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Ok,
    Timeout,
    Error,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RunResult {
    pub status: RunStatus,
    /// `None` when the process was killed by a signal (e.g. the parent SIGKILL).
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    /// Address-space cap in MiB actually applied; `None` when prlimit is
    /// missing from PATH and the run proceeded uncapped.
    pub memory_cap_mb: Option<u64>,
}

/// Resolves the tardigrade binary: `TARDIGRADE_BIN` env, else the
/// `--tardigrade=<path>` flag, else the default checkout next to this one.
pub fn resolve_bin(flag: Option<&str>) -> PathBuf {
    if let Some(bin) = std::env::var_os("TARDIGRADE_BIN") {
        return PathBuf::from(bin);
    }
    if let Some(bin) = flag {
        return PathBuf::from(bin);
    }
    PathBuf::from("../tardigrade/target/release/tardigrade")
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

pub struct Runner {
    bin: PathBuf,
    /// Only set by a successful `--sandbox-check`; every other state refuses runs.
    sandbox_ok: bool,
    prlimit: Option<PathBuf>,
    as_cap_mb: u64,
}

impl Runner {
    pub fn new(bin: PathBuf) -> Result<Runner, String> {
        let as_cap_mb = match std::env::var("TARDIGRADE_RUNNER_AS") {
            Ok(v) => v
                .parse::<u64>()
                .map_err(|e| format!("bad TARDIGRADE_RUNNER_AS {v:?}: {e} (expected MiB)"))?,
            Err(_) => DEFAULT_AS_CAP_MB,
        };
        Ok(Runner {
            bin,
            sandbox_ok: false,
            prlimit: find_in_path("prlimit"),
            as_cap_mb,
        })
    }

    pub fn bin(&self) -> &Path {
        &self.bin
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

    pub fn run_js(&self, code: &str, opts: &RunOptions) -> Result<RunResult, String> {
        if !self.sandbox_ok {
            return Err("tardigrade --sandbox-check has not passed; refusing to run code".into());
        }
        if opts.timeout_ms == 0 || opts.timeout_ms > MAX_TIMEOUT_MS {
            return Err(format!("timeout_ms must be in 1..={MAX_TIMEOUT_MS}"));
        }
        if opts.clock != "virtual" && opts.clock != "real" {
            return Err(r#"clock must be "virtual" or "real""#.into());
        }
        let script = write_temp_script(code)?;
        let result = self.spawn_and_wait(&script, opts);
        let _ = fs::remove_file(&script);
        result
    }

    fn spawn_and_wait(&self, script: &Path, opts: &RunOptions) -> Result<RunResult, String> {
        let start = Instant::now();
        let mut child = self
            .build_command(script, opts)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot spawn tardigrade: {e}"))?;

        // The guest's own --timeout (same value) fires first; the parent only
        // SIGKILLs after a grace period, for the cases the guest watchdog
        // cannot stop (a write blocked on a full pipe, a stuck thread).
        let deadline = Duration::from_millis(u64::from(opts.timeout_ms) + KILL_GRACE_MS);
        let (status, killed_by_us) = wait_with_deadline(&mut child, deadline)?;
        let stdout = read_pipe(child.stdout.take());
        let stderr = read_pipe(child.stderr.take());
        let (status_kind, exit_code) = classify(status, killed_by_us);

        Ok(RunResult {
            status: status_kind,
            exit_code,
            stdout,
            stderr,
            duration_ms: start.elapsed().as_millis(),
            memory_cap_mb: self.prlimit.is_some().then_some(self.as_cap_mb),
        })
    }

    fn build_command(&self, script: &Path, opts: &RunOptions) -> Command {
        let mut guest_args: Vec<String> = vec!["--sandbox".to_owned()];
        if opts.clock == "virtual" {
            guest_args.push("--clock=virtual".to_owned());
        }
        guest_args.push(format!("--seed={}", opts.seed));
        guest_args.push(format!("--timeout={}", opts.timeout_ms));
        guest_args.push(script.to_string_lossy().into_owned());

        match &self.prlimit {
            Some(prlimit) => {
                let mut cmd = Command::new(prlimit);
                // `--` keeps prlimit from parsing tardigrade's own flags.
                cmd.arg(format!("--as={}", self.as_cap_mb * 1024 * 1024));
                cmd.arg("--").arg(&self.bin).args(&guest_args);
                cmd
            }
            None => {
                let mut cmd = Command::new(&self.bin);
                cmd.args(&guest_args);
                cmd
            }
        }
    }
}

static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);

fn write_temp_script(code: &str) -> Result<PathBuf, String> {
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "tardigrade-runner-{}-{}.js",
        std::process::id(),
        seq
    ));
    fs::write(&path, code).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn wait_with_deadline(child: &mut Child, deadline: Duration) -> Result<(ExitStatus, bool), String> {
    let end = Instant::now() + deadline;
    loop {
        let status = child
            .try_wait()
            .map_err(|e| format!("waiting for tardigrade failed: {e}"))?;
        if let Some(status) = status {
            return Ok((status, false));
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let status = child
                .wait()
                .map_err(|e| format!("reaping killed tardigrade failed: {e}"))?;
            return Ok((status, true));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn classify(status: ExitStatus, killed_by_us: bool) -> (RunStatus, Option<i32>) {
    if killed_by_us {
        return (RunStatus::Timeout, None);
    }
    match status.code() {
        Some(0) => (RunStatus::Ok, Some(0)),
        Some(124) => (RunStatus::Timeout, Some(124)),
        Some(code) => (RunStatus::Error, Some(code)),
        None => (RunStatus::Error, None),
    }
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}
