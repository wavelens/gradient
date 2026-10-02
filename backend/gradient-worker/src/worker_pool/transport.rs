/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_util::sync::Mutex;
use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, trace, warn};

use gradient_eval::ipc::{
    DiscoveryShard, EVAL_IPC_VERSION, EvalRequest, EvalResponse, MAX_FRAME_BYTES, ResolvedItem,
    decode_response, encode_request,
};
use gradient_eval::stats::StatsDelta;

/// The stack size is matching upstream Nix's `initNix` `setStackSize(64 MiB)`.
/// The libstdc++ `std::regex` executor behind `builtins.match` is overflowing 8 MiB stacks.
const EVAL_WORKER_STACK_BYTES: u64 = 64 * 1024 * 1024;

const EVAL_WORKER_OOM_SCORE_ADJ: &str = "600";

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

const EXIT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct Listing {
    pub(super) attrs: Vec<String>,
    pub(super) deferred: Vec<DiscoveryShard>,
    pub(super) warnings: Vec<String>,
    pub(super) errors: Vec<String>,
    pub(super) stats: Option<StatsDelta>,
}

pub(super) struct EvalWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// A worker dropped mid-exchange is leaving a stale frame on the wire.
    /// The pool must discard it, or the next request would read that frame as its answer.
    in_flight: bool,
    pid_guard: PidGuard,
}

pub(super) struct PidGuard {
    pub(super) live: Option<Arc<Mutex<HashSet<u32>>>>,
    pub(super) pid: Option<u32>,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let (Some(live), Some(pid)) = (&self.live, self.pid) {
            live.lock().remove(&pid);
        }
    }
}

impl EvalWorker {
    pub(super) async fn spawn(
        eval_cache_dir: &str,
        live: Arc<Mutex<HashSet<u32>>>,
    ) -> Result<Self> {
        let exe = std::env::current_exe().context("locating current executable")?;
        trace!(exe = %exe.display(), "spawning eval worker subprocess");
        let mut command = Command::new(&exe);
        command.arg("--eval-subprocess");
        command.env("NIX_CACHE_HOME", eval_cache_dir);
        if let Some(dir) = gradient_util::trace_file::active_dir() {
            command.env("GRADIENT_WORKER_LOG_TRACE_DIR", dir);
        }
        for &(k, v) in
            super::eval_stats::eval_worker_stats_env(super::eval_stats::metrics_enabled())
        {
            command.env(k, v);
        }

        // SAFETY: `pre_exec` is running in the forked child before `exec`.
        // Its body must be async-signal-safe.
        // It is only building an `rlimit` and calling `setrlimit`, both signal-safe.
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

        if let Some(pid) = worker.pid_guard.pid {
            live.lock().insert(pid);
        }
        worker.pid_guard.live = Some(live);

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
            in_flight: false,
            pid_guard: PidGuard { live: None, pid },
        })
    }

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

    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub(super) fn in_flight(&self) -> bool {
        self.in_flight
    }

    async fn send(&mut self, req: &EvalRequest) -> Result<()> {
        self.in_flight = true;
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

    async fn call<T>(
        &mut self,
        req: EvalRequest,
        what: &'static str,
        extract: impl FnOnce(EvalResponse) -> std::result::Result<T, Box<EvalResponse>>,
    ) -> Result<T> {
        self.send(&req).await?;
        let resp = self.recv().await?;
        self.in_flight = false;
        match extract(resp) {
            Ok(v) => Ok(v),
            Err(other) => match *other {
                EvalResponse::Err { message } => Err(anyhow::anyhow!("eval worker: {message}")),
                other => anyhow::bail!("eval worker: unexpected response to {what}: {other:?}"),
            },
        }
    }

    pub(super) async fn plan(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
        input_overrides: Vec<(String, String)>,
    ) -> Result<(Vec<DiscoveryShard>, Vec<String>)> {
        self.call(
            EvalRequest::Plan {
                repository,
                wildcards,
                input_overrides,
            },
            "Plan",
            |resp| match resp {
                EvalResponse::PlanOk { shards, errors } => Ok((shards, errors)),
                other => Err(Box::new(other)),
            },
        )
        .await
    }

    pub(super) async fn list(
        &mut self,
        repository: String,
        wildcards: Vec<String>,
        only: Option<Vec<String>>,
        input_overrides: Vec<(String, String)>,
    ) -> Result<Listing> {
        self.call(
            EvalRequest::List {
                repository,
                wildcards,
                only,
                input_overrides,
            },
            "List",
            |resp| match resp {
                EvalResponse::ListOk {
                    attrs,
                    deferred,
                    warnings,
                    errors,
                    stats,
                } => Ok(Listing {
                    attrs,
                    deferred,
                    warnings,
                    errors,
                    stats,
                }),
                other => Err(Box::new(other)),
            },
        )
        .await
    }

    pub(super) async fn fingerprint(
        &mut self,
        repository: String,
        input_overrides: Vec<(String, String)>,
    ) -> Result<Option<String>> {
        self.call(
            EvalRequest::Fingerprint {
                repository,
                input_overrides,
            },
            "Fingerprint",
            |resp| match resp {
                EvalResponse::FingerprintOk { fingerprint } => Ok(fingerprint),
                other => Err(Box::new(other)),
            },
        )
        .await
    }

    pub(super) async fn checkpoint(
        &mut self,
        repository: String,
        input_overrides: Vec<(String, String)>,
    ) -> Result<()> {
        self.call(
            EvalRequest::Checkpoint {
                repository,
                input_overrides,
            },
            "Checkpoint",
            |resp| match resp {
                EvalResponse::CheckpointOk => Ok(()),
                other => Err(Box::new(other)),
            },
        )
        .await
    }

    pub(super) async fn resolve(
        &mut self,
        repository: String,
        attrs: Vec<String>,
        input_overrides: Vec<(String, String)>,
    ) -> (Vec<ResolvedItem>, Result<(Vec<String>, Option<StatsDelta>)>) {
        let mut items = Vec::new();
        let end = self
            .resolve_inner(repository, attrs, input_overrides, &mut items)
            .await;
        (items, end)
    }

    async fn resolve_inner(
        &mut self,
        repository: String,
        attrs: Vec<String>,
        input_overrides: Vec<(String, String)>,
        items: &mut Vec<ResolvedItem>,
    ) -> Result<(Vec<String>, Option<StatsDelta>)> {
        self.send(&EvalRequest::Resolve {
            repository,
            attrs,
            input_overrides,
        })
        .await?;
        loop {
            match self.recv().await? {
                EvalResponse::ResolveItem { item } => items.push(item),
                EvalResponse::ResolveEnd { warnings, stats } => {
                    self.in_flight = false;
                    return Ok((warnings, stats));
                }
                EvalResponse::Err { message } => {
                    self.in_flight = false;
                    anyhow::bail!("eval worker: {message}")
                }
                other => {
                    anyhow::bail!("eval worker: unexpected response to Resolve: {other:?}")
                }
            }
        }
    }

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
