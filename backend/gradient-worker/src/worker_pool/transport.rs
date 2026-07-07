/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Parent-side handle to one eval-worker subprocess: spawn, the rkyv frame
//! transport over its stdin/stdout, and the typed request methods. Pool
//! lifecycle lives in [`super::pool`], memory accounting in [`super::memory`].

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, trace, warn};

use gradient_eval::ipc::{
    EVAL_IPC_VERSION, EvalRequest, EvalResponse, MAX_FRAME_BYTES, ResolvedItem, decode_response,
    encode_request,
};
use gradient_eval::stats::StatsDelta;

/// Stack size for the subprocess, matching upstream Nix's `initNix`
/// `setStackSize(64 MiB)`: libnix's libstdc++ `std::regex` DFS executor (used
/// by `builtins.match` / `builtins.split`) overflows the default 8 MiB on
/// deep patterns.
const EVAL_WORKER_STACK_BYTES: u64 = 64 * 1024 * 1024;

/// `oom_score_adj` for eval subprocesses: the kernel sacrifices them (large
/// Nix/Boehm-GC heaps) before the parent worker or other services.
const EVAL_WORKER_OOM_SCORE_ADJ: &str = "600";

/// How long `spawn` waits for the subprocess's version byte. Covers exec +
/// Rust init only; the slow libnix init happens after the handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Grace period for a `Shutdown`n worker to run libnix's atexit handlers
/// (flush eval-cache SQLite, release locks) before it is SIGKILL'd.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How long the dead-pipe diagnostics wait for the child's exit status before
/// falling back to `/proc` state sampling.
const EXIT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Handle to a single live eval-worker subprocess.
///
/// Owns the child plus its piped stdin/stdout. The wire is `u32` LE length +
/// rkyv frames ([`gradient_eval::ipc`]); every request is one frame, every
/// response one frame, except `Resolve` which streams item frames until a
/// terminating `ResolveEnd`.
pub(super) struct EvalWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// RAII deregistration of this subprocess's pid from the pool's live
    /// registry. A field (rather than `impl Drop for EvalWorker`) so `shutdown`
    /// can still move individual fields out of `self`.
    pid_guard: PidGuard,
}

/// Removes an eval subprocess pid from the pool's live registry on drop, so the
/// memory reaper never targets a worker we have already discarded. The child
/// itself is reaped by `kill_on_drop`.
pub(super) struct PidGuard {
    /// Live-pid registry of the owning pool. `None` for test workers built via
    /// `from_command` (no pool, nothing to deregister from).
    pub(super) live: Option<Arc<Mutex<HashSet<u32>>>>,
    pub(super) pid: Option<u32>,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let (Some(live), Some(pid)) = (&self.live, self.pid) {
            live.lock().unwrap().remove(&pid);
        }
    }
}

impl EvalWorker {
    /// Spawn a new worker by re-execing the current binary with `--eval-worker`
    /// and verify its IPC version byte. The subprocess is single-threaded and
    /// does not fork; pool size is the eval concurrency and RSS is bounded
    /// parent-side (see [`Self::rss_bytes`]).
    ///
    /// `eval_cache_dir` is exported as `NIX_CACHE_HOME` so parent and worker
    /// agree on where Nix's `eval-cache-v6/<fingerprint>.sqlite` lives.
    pub(super) async fn spawn(
        eval_cache_dir: &str,
        live: Arc<Mutex<HashSet<u32>>>,
    ) -> Result<Self> {
        let exe = std::env::current_exe().context("locating current executable")?;
        trace!(exe = %exe.display(), "spawning eval worker subprocess");
        let mut command = Command::new(&exe);
        command.arg("--eval-worker");
        command.env("NIX_CACHE_HOME", eval_cache_dir);
        for &(k, v) in
            super::eval_stats::eval_worker_stats_env(super::eval_stats::metrics_enabled())
        {
            command.env(k, v);
        }

        // SAFETY: `pre_exec` runs in the forked child before `exec`, so its body
        // must be async-signal-safe; it only builds an `rlimit` and calls
        // `setrlimit`, both of which are signal-safe.
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                let lim = libc::rlimit {
                    rlim_cur: EVAL_WORKER_STACK_BYTES,
                    rlim_max: EVAL_WORKER_STACK_BYTES,
                };
                if libc::setrlimit(libc::RLIMIT_STACK, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut worker = Self::from_command(command)?;

        // Register the pid so the memory reaper can find this subprocess even
        // while it is checked out of the pool. Deregistered by `PidGuard`.
        if let Some(pid) = worker.pid_guard.pid {
            live.lock().unwrap().insert(pid);
        }
        worker.pid_guard.live = Some(live);

        // Mark the subprocess as the preferred OOM-kill target. A cheap
        // sub-page procfs write; not worth a spawn_blocking hop.
        #[cfg(target_os = "linux")]
        if let Some(pid) = worker.child.id() {
            let path = format!("/proc/{pid}/oom_score_adj");
            if let Err(e) = std::fs::write(&path, EVAL_WORKER_OOM_SCORE_ADJ) {
                warn!(pid, error = %e, "failed to set oom_score_adj for eval worker");
            }
        }

        worker.expect_handshake().await?;

        Ok(worker)
    }

    /// Spawn the given pre-configured command and wrap its stdin/stdout into
    /// an [`EvalWorker`]. Test seam used by pool tests to stand up a
    /// controllable subprocess (e.g. `cat`) without depending on libnix; no
    /// version handshake is performed here.
    pub(super) fn from_command(mut command: Command) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn().context("spawning eval worker subprocess")?;
        let pid = child.id();
        let stdin = child.stdin.take().context("worker stdin missing")?;
        let stdout = BufReader::new(child.stdout.take().context("worker stdout missing")?);
        Ok(Self {
            child,
            stdin,
            stdout,
            pid_guard: PidGuard { live: None, pid },
        })
    }

    /// Read and verify the one-byte version handshake the subprocess writes
    /// before its first frame, so a binary swapped mid-run fails loudly here
    /// instead of as undecodable frames later.
    async fn expect_handshake(&mut self) -> Result<()> {
        let mut version = [0u8; 1];
        tokio::time::timeout(HANDSHAKE_TIMEOUT, self.stdout.read_exact(&mut version))
            .await
            .context("eval worker handshake timed out")?
            .context("reading eval worker handshake")?;
        anyhow::ensure!(
            version[0] == EVAL_IPC_VERSION,
            "eval worker IPC version mismatch: parent {EVAL_IPC_VERSION}, subprocess {} (binary replaced mid-run?)",
            version[0]
        );
        Ok(())
    }

    pub(super) fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Whether the subprocess is still running, reaping its exit status if it
    /// has already exited. The pool calls this on checkout to discard an idle
    /// worker whose subprocess died while pooled (memory reaper, kernel OOM via
    /// the elevated `oom_score_adj`, or crash) instead of handing out a corpse
    /// that fails the next write with a broken pipe.
    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Write one request frame. An error means the worker is no longer usable.
    async fn send(&mut self, req: &EvalRequest) -> Result<()> {
        trace!(pid = self.child.id(), ?req, "sending eval worker request");
        let payload = encode_request(req).context("encoding eval worker request")?;
        self.stdin
            .write_all(&u32::try_from(payload.len())?.to_le_bytes())
            .await
            .context("writing to eval worker stdin")?;
        self.stdin
            .write_all(&payload)
            .await
            .context("writing to eval worker stdin")?;
        self.stdin
            .flush()
            .await
            .context("flushing eval worker stdin")
    }

    /// Read one response frame. An error means the worker is no longer usable
    /// (the caller marks it dead so it gets discarded instead of pooled).
    async fn recv(&mut self) -> Result<EvalResponse> {
        let mut len_buf = [0u8; 4];
        if let Err(e) = self.stdout.read_exact(&mut len_buf).await {
            anyhow::bail!("eval worker closed pipe ({})", self.describe_death(e).await);
        }

        let len = u32::from_le_bytes(len_buf);
        anyhow::ensure!(
            len <= MAX_FRAME_BYTES,
            "eval worker frame length {len} exceeds MAX_FRAME_BYTES (corrupt stream?)"
        );

        let mut payload = vec![0u8; len as usize];
        self.stdout
            .read_exact(&mut payload)
            .await
            .context("reading eval worker response frame")?;

        trace!(
            pid = self.child.id(),
            bytes = payload.len(),
            "received eval worker response"
        );
        decode_response(&payload).context("decoding eval worker response")
    }

    /// Best-effort post-mortem for a dead read side: exit status if the child
    /// is gone, `/proc` state samples if it is somehow still alive. Sub-page
    /// procfs reads, cheap enough to stay on the async path.
    async fn describe_death(&mut self, read_err: std::io::Error) -> String {
        let pid = self.child.id();
        let status = match tokio::time::timeout(EXIT_PROBE_TIMEOUT, self.child.wait()).await {
            Ok(Ok(s)) => format!("{s}"),
            Ok(Err(e)) => format!("wait error: {e}"),
            Err(_) => {
                let mut diag = String::from("still alive after 2s");
                if let Some(p) = pid {
                    if let Ok(target) = std::fs::read_link(format!("/proc/{p}/fd/1")) {
                        diag.push_str(&format!("; /proc/{p}/fd/1 -> {}", target.display()));
                    }
                    if let Ok(state) = std::fs::read_to_string(format!("/proc/{p}/status"))
                        && let Some(line) = state.lines().find(|l| l.starts_with("State:"))
                    {
                        diag.push_str(&format!("; {line}"));
                    }
                    if let Ok(wchan) = std::fs::read_to_string(format!("/proc/{p}/wchan")) {
                        diag.push_str(&format!("; wchan={}", wchan.trim()));
                    }
                }
                diag
            }
        };
        format!("pid={pid:?}, read error={read_err}, exit={status}")
    }

    /// One request, one typed response. `extract` returns the unexpected
    /// response so the single shape-check here can name it; `EvalResponse::Err`
    /// becomes the worker's own error message.
    async fn call<T>(
        &mut self,
        req: EvalRequest,
        what: &'static str,
        extract: impl FnOnce(EvalResponse) -> std::result::Result<T, EvalResponse>,
    ) -> Result<T> {
        self.send(&req).await?;
        match extract(self.recv().await?) {
            Ok(v) => Ok(v),
            Err(EvalResponse::Err { message }) => Err(anyhow::anyhow!("eval worker: {message}")),
            Err(other) => anyhow::bail!("eval worker: unexpected response to {what}: {other:?}"),
        }
    }

    pub(super) async fn plan(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        self.call(
            EvalRequest::Plan {
                repository,
                wildcards,
            },
            "Plan",
            |resp| match resp {
                EvalResponse::PlanOk { sub_patterns, errors } => Ok((sub_patterns, errors)),
                other => Err(other),
            },
        )
        .await
    }

    pub(super) async fn list(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>, Vec<String>, Option<StatsDelta>)> {
        self.call(
            EvalRequest::List {
                repository,
                wildcards,
            },
            "List",
            |resp| match resp {
                EvalResponse::ListOk {
                    attrs,
                    warnings,
                    errors,
                    stats,
                } => Ok((attrs, warnings, errors, stats)),
                other => Err(other),
            },
        )
        .await
    }

    pub(super) async fn fingerprint(&mut self, repository: String) -> Result<Option<String>> {
        self.call(
            EvalRequest::Fingerprint { repository },
            "Fingerprint",
            |resp| match resp {
                EvalResponse::FingerprintOk { fingerprint } => Ok(fingerprint),
                other => Err(other),
            },
        )
        .await
    }

    pub(super) async fn checkpoint(&mut self, repository: String) -> Result<()> {
        self.call(
            EvalRequest::Checkpoint { repository },
            "Checkpoint",
            |resp| match resp {
                EvalResponse::CheckpointOk => Ok(()),
                other => Err(other),
            },
        )
        .await
    }

    /// Resolve `attrs`, returning every item streamed before the terminal
    /// result. `Ok` carries the batch's warnings + stats delta; `Err` means
    /// the subprocess died mid-stream, in which case the streamed prefix is
    /// still valid and the first unstreamed attr is the crash suspect.
    pub(super) async fn resolve(
        &mut self,
        repository: String,
        attrs: Vec<String>,
    ) -> (
        Vec<ResolvedItem>,
        Result<(Vec<String>, Option<StatsDelta>)>,
    ) {
        let mut items = Vec::new();
        let end = self.resolve_inner(repository, attrs, &mut items).await;
        (items, end)
    }

    async fn resolve_inner(
        &mut self,
        repository: String,
        attrs: Vec<String>,
        items: &mut Vec<ResolvedItem>,
    ) -> Result<(Vec<String>, Option<StatsDelta>)> {
        self.send(&EvalRequest::Resolve { repository, attrs })
            .await?;
        loop {
            match self.recv().await? {
                EvalResponse::ResolveItem { item } => items.push(item),
                EvalResponse::ResolveEnd { warnings, stats } => return Ok((warnings, stats)),
                EvalResponse::Err { message } => {
                    anyhow::bail!("eval worker: {message}")
                }
                other => {
                    anyhow::bail!("eval worker: unexpected response to Resolve: {other:?}")
                }
            }
        }
    }

    /// Send a `Shutdown` request and wait briefly for the child to exit.
    /// Used when the parent is recycling a still-healthy worker so the
    /// subprocess can run libnix's atexit handlers (flush eval-cache
    /// SQLite, release locks, drop temp roots) instead of being SIGKILL'd
    /// by `kill_on_drop`.
    pub(super) async fn shutdown(mut self) {
        let pid = self.child.id();
        trace!(pid, "sending Shutdown to eval worker");
        if let Err(e) = self.send(&EvalRequest::Shutdown).await {
            debug!(pid, error = %e, "failed to write Shutdown to eval worker");
            return;
        }
        drop(self.stdin);
        trace!(pid, "Shutdown sent; waiting for eval worker to exit");
        match tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait()).await {
            Ok(Ok(status)) => trace!(pid, ?status, "eval worker exited cleanly"),
            Ok(Err(e)) => debug!(pid, error = %e, "waiting on eval worker exit failed"),
            Err(_) => warn!(
                pid,
                "eval worker did not exit within the shutdown grace period; will be killed"
            ),
        }
    }

    /// Resident set size of the subprocess in bytes. Returns 0 if the pid is
    /// gone or the read fails so the pool never panics on it.
    pub(super) fn rss_bytes(&self) -> u64 {
        self.child
            .id()
            .and_then(super::memory::rss_of_pid)
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(super) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl std::fmt::Debug for EvalWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvalWorker")
            .field("pid", &self.child.id())
            .finish()
    }
}
