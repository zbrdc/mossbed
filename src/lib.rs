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

/// Address-space cap (MiB) applied with `prlimit --as`.
///
/// V8 aborts at startup below ~1.3 GiB of address space because it reserves
/// the pointer-compression cage and the code range up front (verified against
/// the real binary: `--as=512MiB` dies with "Check failed: Start()").
/// 2 GiB lets V8 start while still bounding a runaway allocator.
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
/// at roughly 10 500 frames.
///
/// This is V8 stack size, chosen by the embedder, so it is not adjustable from
/// here. Guest code with a recursive descent deeper than that must be made
/// iterative; the failure is a RangeError the guest can catch rather than a
/// crash, so the shape of the failure is at least predictable.
pub const GUEST_RECURSION_LIMIT_HINT: u32 = 10_500;

/// Address space V8 occupies before any guest byte is allocated.
///
/// Measured, not guessed. With this floor at 1400 and a 1024 MiB heap the belt
/// came to 2424 MiB and the guest still stopped at 816 MiB: V8 held the rest.
/// 1800 is that measurement with slack, so the heap cap the operator asks for
/// is the cap that binds, rather than the belt quietly binding first. V8 aborts
/// at startup below roughly 1.3 GiB, so a floor this high is also a floor for
/// the default.
const AS_FLOOR_MB: u64 = 1800;

/// Extra time past the guest's own `--timeout` before the parent SIGKILLs,
/// so tardigrade's clean exit 124 usually wins over a raw kill.
const KILL_GRACE_MS: u64 = 500;

/// A run must always be bounded from the parent side; one hour is plenty.
const MAX_TIMEOUT_MS: u32 = 3_600_000;

/// Per-stream capture cap while draining a running guest.
pub const DRAIN_CAP_BYTES: usize = 64 * 1024;

/// Largest end-of-run dump returned inline, in bytes (TARDIGRADE_RUNNER_DUMP_MAX).
pub const DEFAULT_DUMP_MAX_BYTES: u64 = 1024 * 1024;

/// Appended to a stream whose drain hit [`DRAIN_CAP_BYTES`].
pub const TRUNCATED_MARKER: &str = "…[truncated]";

#[derive(Debug)]
pub struct RunOptions {
    pub timeout_ms: u32,
    pub seed: u32,
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
    /// End-of-run filesystem snapshot when dump was requested and the run
    /// wrote one; None otherwise (dump not requested, or the run ended before
    /// any state existed). Feed it back as preload to continue a workflow.
    pub dump: Option<String>,
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

/// Fail closed when prlimit is missing: an uncapped guest can exhaust host
/// memory, so only TARDIGRADE_ALLOW_UNCAPPED=1 explicitly accepts that risk.
fn uncapped_run_refusal(prlimit_missing: bool, allow_uncapped: bool) -> Option<&'static str> {
    (prlimit_missing && !allow_uncapped).then_some(
        "prlimit not found on PATH; refusing uncapped run (set TARDIGRADE_ALLOW_UNCAPPED=1 to override)",
    )
}

pub struct Runner {
    bin: PathBuf,
    /// Only set by a successful `--sandbox-check`; every other state refuses runs.
    sandbox_ok: bool,
    prlimit: Option<PathBuf>,
    as_cap_mb: u64,
    heap_mb: u64,
    /// Largest dump returned inline; oversize dumps are suppressed with a
    /// stderr note rather than ballooning an MCP message.
    dump_max_bytes: u64,
}

impl Runner {
    pub fn new(bin: PathBuf) -> Result<Runner, String> {
        let as_cap_mb = match std::env::var("TARDIGRADE_RUNNER_AS") {
            Ok(v) => v
                .parse::<u64>()
                .map_err(|e| format!("bad TARDIGRADE_RUNNER_AS {v:?}: {e} (expected MiB)"))?,
            Err(_) => DEFAULT_AS_CAP_MB,
        };
        let heap_mb = match std::env::var("TARDIGRADE_RUNNER_MEM") {
            Ok(v) => v
                .parse::<u64>()
                .map_err(|e| format!("bad TARDIGRADE_RUNNER_MEM {v:?}: {e} (expected MiB)"))?,
            Err(_) => DEFAULT_HEAP_MB,
        };
        let dump_max_bytes = match std::env::var("TARDIGRADE_RUNNER_DUMP_MAX") {
            Ok(v) => v
                .parse::<u64>()
                .map_err(|e| format!("bad TARDIGRADE_RUNNER_DUMP_MAX {v:?}: {e} (expected bytes)"))?,
            Err(_) => DEFAULT_DUMP_MAX_BYTES,
        };
        Ok(Runner {
            bin,
            sandbox_ok: false,
            prlimit: find_in_path("prlimit"),
            as_cap_mb,
            heap_mb,
            dump_max_bytes,
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
            // Seq-unique names: the script dir is shared by concurrent runs
            // in this process, so fixed names would collide (File exists).
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
        let mut result =
            self.spawn_and_wait(&script, preload_path.as_deref(), dump_path.as_deref(), opts)?;
        if let Some(dump_file) = dump_path.as_deref() {
            let len = fs::metadata(dump_file).map(|m| m.len()).unwrap_or(0);
            if len > self.dump_max_bytes {
                result.stderr = format!(
                    "{}tardigrade-runner: dump suppressed ({} bytes > {} cap); keep handed-off state small or raise TARDIGRADE_RUNNER_DUMP_MAX\n",
                    result.stderr, len, self.dump_max_bytes
                );
            } else {
                result.dump = fs::read_to_string(dump_file)
                    .ok()
                    .filter(|content| !content.is_empty());
            }
        }
        Ok(result)
    }

    fn spawn_and_wait(
        &self,
        script: &Path,
        preload: Option<&Path>,
        dump: Option<&Path>,
        opts: &RunOptions,
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
        let (status, killed_by_us) = wait_with_deadline(&mut child, deadline)?;
        let stdout = stdout_thread
            .join()
            .map_err(|_| "stdout drain thread panicked".to_owned())?;
        let stderr = stderr_thread
            .join()
            .map_err(|_| "stderr drain thread panicked".to_owned())?;
        let stderr = with_abort_note(stderr);
        let (status_kind, exit_code) = classify(status, killed_by_us);

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
            dump: None,
        })
    }

    fn build_command(
        &self,
        script: &Path,
        preload: Option<&Path>,
        dump: Option<&Path>,
        opts: &RunOptions,
    ) -> Command {
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
        guest_args.push(format!("--timeout={}", opts.timeout_ms));
        guest_args.push(script.to_string_lossy().into_owned());

        match &self.prlimit {
            Some(prlimit) => {
                let mut cmd = Command::new(prlimit);
                // `--` keeps prlimit from parsing tardigrade's own flags.
                cmd.arg(format!("--as={}", self.effective_as_mb() * 1024 * 1024));
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

/// A guest can abort its own process instead of throwing. V8's fatal errors name
/// the subsystem; where the caller can act on that, append a note, leaving the
/// original stderr intact above it.
fn with_abort_note(stderr: String) -> String {
    let note = if stderr.contains("DateTimePatternGeneratorCache") || stderr.contains("Icu error") {
        "tardigrade-runner: this is the known ICU bug, not your script. Intl date formatting (Intl.DateTimeFormat, Date.prototype.toLocale*String, resolvedOptions) is missing from this build and aborts the guest. Format dates by hand, and pass clock: real if the current time is needed."
    } else if stderr.contains("Fatal process out of memory") {
        "tardigrade-runner: the guest hit a hard memory limit and V8 aborted it. The heap and ArrayBuffer are capped at 256 MiB each by default; exceeding them normally raises a catchable RangeError, so an abort here means V8 ran out of room before guest code could catch the limit."
    } else {
        return stderr;
    };
    format!("{stderr}\n{note}")
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
        // the directory is shared by every Runner in the process, so a Drop that
        // removed it would delete it out from under a concurrent run. One empty
        // directory per server lifetime used to accumulate in /tmp. Both tests
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
}
