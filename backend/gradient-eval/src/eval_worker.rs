/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Long-lived Nix evaluator worker subprocess.
//!
//! The parent process spawns one or more copies of the gradient-worker binary
//! with `--eval-worker` to host a persistent [`NixEvaluator`]. Parent and
//! worker exchange rkyv frames over the worker's stdin/stdout (see
//! [`crate::ipc`]), so the libnix init cost is paid only once per worker.
//!
//! This file is the subprocess entry point [`run_eval_worker`]; the protocol
//! lives in [`crate::ipc`] and the parent-side pool in the worker crate.

use std::io::Write;
use tracing::{error, trace};

use crate::flake_walk::FlakeWalker;
use crate::ipc::{
    EVAL_IPC_VERSION, EvalRequest, EvalResponse, ResolvedItem, decode_request, encode_response,
    read_frame, write_frame,
};
use crate::nix_eval::NixEvaluator;
use crate::stats::StatsDelta;

/// Subprocess entry point. Reads [`EvalRequest`] frames from stdin, processes
/// them with one persistent [`NixEvaluator`], writes [`EvalResponse`] frames
/// to stdout. Returns when stdin reaches EOF or a `Shutdown` request arrives.
///
/// Diagnostics go through `tracing` (configured in `worker::main` to write
/// formatted records to stderr, which the parent inherits) so init failures
/// and stdin errors stay visible to JSON log aggregators with structured
/// fields, target metadata, and `RUST_LOG` filtering applied.
pub fn run_eval_worker() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    // Version handshake before the (slow) evaluator init, so the parent can
    // reject a mid-run binary swap without waiting on libnix.
    writer.write_all(&[EVAL_IPC_VERSION])?;
    writer.flush()?;

    // Construct the evaluator once. If init fails we still loop so that the
    // parent gets an error response per request instead of a silent EOF; the
    // parent will then mark this worker dead and respawn.
    let evaluator = match NixEvaluator::new() {
        Ok(e) => Some(e),
        Err(e) => {
            error!(
                error = format!("{e:#}"),
                "eval worker: NixEvaluator init failed"
            );
            None
        }
    };

    // Per-request delta collection is on by default; disabling it skips every
    // `ev.stats()` call so the subprocess pays zero overhead.
    let collect_stats = crate::stats::metrics_enabled();
    let mut last = if collect_stats {
        evaluator
            .as_ref()
            .and_then(|ev| ev.stats().ok())
            .unwrap_or_default()
    } else {
        nix_bindings::EvalStats::default()
    };

    // Reads the cumulative counters, returns the delta since the prior request,
    // and advances the baseline. `None` when stats collection is disabled.
    let mut take_delta = |ev: &NixEvaluator| -> Option<StatsDelta> {
        if !collect_stats {
            return None;
        }

        ev.stats().ok().map(|cur| {
            let d = cur.saturating_sub(&last);
            let heap = cur.gc_heap_size;
            last = cur;
            StatsDelta {
                nr_thunks: d.nr_thunks,
                nr_function_calls: d.nr_function_calls,
                nr_primop_calls: d.nr_primop_calls,
                nr_lookups: d.nr_lookups,
                alloc_bytes: d.gc_total_bytes,
                gc_heap_size: heap,
            }
        })
    };

    // One walker (locked flake + open eval cache) reused across consecutive
    // requests for the same repository, so a Plan/List/Resolve sequence pays
    // the lock + cache open once. Writes are committed to the WAL after every
    // request, so holding it open never withholds progress from shard peers.
    let mut walkers = WalkerCache { entry: None };

    loop {
        let Some(payload) = read_frame(&mut reader).inspect_err(|e| {
            error!(error = %e, "eval worker: stdin read error");
        })?
        else {
            // EOF: parent closed the pipe.
            return Ok(());
        };

        let req = match decode_request(&payload) {
            Ok(r) => r,
            Err(e) => {
                // The length prefix kept the stream aligned, so an undecodable
                // payload is per-request recoverable: report and keep serving.
                send(
                    &mut writer,
                    &EvalResponse::Err {
                        message: format!("malformed request: {e}"),
                    },
                )?;
                continue;
            }
        };

        trace!(?req, "eval worker received request");
        let resp = match req {
            EvalRequest::Shutdown => {
                trace!("eval worker shutting down on request");
                return Ok(());
            }
            EvalRequest::Plan {
                repository,
                wildcards,
            } => with_evaluator(&evaluator, |ev| {
                // Warnings from priming the prefix attrset resurface when each
                // shard re-forces it, so they are not captured here; per-attr
                // eval errors are, because a thrown shard root produces no shard
                // for any later `List` to re-hit.
                or_err(walkers.with(ev, &repository, |walker| {
                    let (shards, errors) = walker.plan_shards(&wildcards)?;
                    let _ = walker.commit_cache();
                    Ok((shards, errors))
                })
                .map(|(sub_patterns, errors)| EvalResponse::PlanOk { sub_patterns, errors }))
            }),
            EvalRequest::List {
                repository,
                wildcards,
            } => with_evaluator(&evaluator, |ev| {
                let (result, warnings) = capture_warnings_during(|| {
                    walkers.with(ev, &repository, |walker| {
                        let (attrs, errors) = walker.discover(&wildcards)?;
                        let _ = walker.commit_cache();
                        Ok((attrs, errors))
                    })
                });
                let stats = take_delta(ev);
                or_err(result.map(|(attrs, errors)| EvalResponse::ListOk {
                    attrs,
                    warnings,
                    errors,
                    stats,
                }))
            }),
            EvalRequest::Resolve { repository, attrs } => {
                let resp = match evaluator.as_ref() {
                    None => EvalResponse::Err {
                        message: "evaluator not initialized".to_string(),
                    },
                    Some(ev) => {
                        let (warnings, io) =
                            stream_resolve(&mut writer, ev, &mut walkers, &repository, attrs);
                        // A failed item-frame write means the parent is gone.
                        io?;
                        EvalResponse::ResolveEnd {
                            warnings,
                            stats: take_delta(ev),
                        }
                    }
                };
                send(&mut writer, &resp)?;
                continue;
            }
            EvalRequest::Fingerprint { repository } => with_evaluator(&evaluator, |ev| {
                or_err(
                    ev.fingerprint(&repository)
                        .map(|fingerprint| EvalResponse::FingerprintOk { fingerprint }),
                )
            }),
            EvalRequest::Checkpoint { repository } => with_evaluator(&evaluator, |ev| {
                or_err(walkers
                    .with(ev, &repository, |walker| walker.checkpoint_cache())
                    .map(|()| EvalResponse::CheckpointOk))
            }),
        };

        trace!(kind = response_kind(&resp), "eval worker sending response");
        send(&mut writer, &resp)?;
    }
}

/// Runs `f` against the initialized evaluator, or answers the one canonical
/// error when libnix failed to come up (the parent then discards this worker).
fn with_evaluator<'ev>(
    evaluator: &'ev Option<NixEvaluator>,
    f: impl FnOnce(&'ev NixEvaluator) -> EvalResponse,
) -> EvalResponse {
    match evaluator {
        Some(ev) => f(ev),
        None => EvalResponse::Err {
            message: "evaluator not initialized".to_string(),
        },
    }
}

/// Collapses an operation's error into the wire's `Err` response.
fn or_err(result: anyhow::Result<EvalResponse>) -> EvalResponse {
    result.unwrap_or_else(|e| EvalResponse::Err {
        message: format!("{e:#}"),
    })
}

/// Single-entry walker cache keyed by repository. Consecutive requests for the
/// same flake (the common Plan/List/Resolve sequence) reuse one locked flake +
/// open eval cache; a different repository replaces the entry.
struct WalkerCache<'ev> {
    entry: Option<(String, FlakeWalker<'ev>)>,
}

impl<'ev> WalkerCache<'ev> {
    /// The cached walker for `repository`, opening (and caching) it if absent.
    fn open(
        &mut self,
        ev: &'ev NixEvaluator,
        repository: &str,
    ) -> anyhow::Result<&FlakeWalker<'ev>> {
        let stale = self.entry.as_ref().is_none_or(|(repo, _)| repo != repository);
        if stale {
            // Drop the previous walker before locking the next flake.
            self.entry = None;
            let walker = ev.walker(repository)?;
            self.entry = Some((repository.to_string(), walker));
        }

        Ok(&self.entry.as_ref().expect("entry just ensured").1)
    }

    fn with<T>(
        &mut self,
        ev: &'ev NixEvaluator,
        repository: &str,
        f: impl FnOnce(&FlakeWalker<'ev>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        f(self.open(ev, repository)?)
    }
}

/// Resolve each attr in order, streaming one `ResolveItem` frame per attr the
/// moment it is resolved. Returns the batch's captured warnings plus the IO
/// status of the frame writes. A walker that cannot open becomes one per-attr
/// error item per attr (streamed), never a top-level `Err`, matching the
/// per-attr isolation contract of `Resolve`.
fn stream_resolve<'ev, W: Write>(
    writer: &mut W,
    ev: &'ev NixEvaluator,
    walkers: &mut WalkerCache<'ev>,
    repository: &str,
    attrs: Vec<String>,
) -> (Vec<String>, std::io::Result<()>) {
    let mut all_warnings = Vec::new();
    let mut io = Ok(());
    let emit = |writer: &mut W, io: &mut std::io::Result<()>, item: ResolvedItem| {
        if io.is_ok() {
            *io = send(writer, &EvalResponse::ResolveItem { item });
        }
    };

    let (walker_result, build_warnings) = capture_warnings_during(|| walkers.open(ev, repository));
    all_warnings.extend(build_warnings);

    match walker_result {
        Ok(walker) => {
            for attr in attrs {
                let (result, warnings) = capture_warnings_during(|| walker.resolve(&attr));
                all_warnings.extend(warnings);
                let item = match result {
                    Ok((drv, references)) => ResolvedItem {
                        attr,
                        drv_path: Some(drv),
                        references,
                        error: None,
                    },
                    Err(e) => ResolvedItem {
                        attr,
                        drv_path: None,
                        references: vec![],
                        error: Some(format!("{e:#}")),
                    },
                };
                emit(writer, &mut io, item);
            }

            let _ = walker.commit_cache();
        }
        Err(e) => {
            let msg = format!("{e:#}");
            for attr in attrs {
                emit(
                    writer,
                    &mut io,
                    ResolvedItem {
                        attr,
                        drv_path: None,
                        references: vec![],
                        error: Some(msg.clone()),
                    },
                );
            }
        }
    }

    all_warnings.dedup();
    (all_warnings, io)
}

fn send<W: Write>(w: &mut W, resp: &EvalResponse) -> std::io::Result<()> {
    let payload = encode_response(resp).map_err(std::io::Error::other)?;
    write_frame(w, &payload)
}

fn response_kind(resp: &EvalResponse) -> String {
    match resp {
        EvalResponse::PlanOk { sub_patterns, .. } => format!("PlanOk({} shards)", sub_patterns.len()),
        EvalResponse::ListOk { attrs, .. } => format!("ListOk({} attrs)", attrs.len()),
        EvalResponse::ResolveItem { item } => format!("ResolveItem({})", item.attr),
        EvalResponse::ResolveEnd { warnings, .. } => {
            format!("ResolveEnd({} warnings)", warnings.len())
        }
        EvalResponse::FingerprintOk { fingerprint } => {
            format!("FingerprintOk({})", fingerprint.is_some())
        }
        EvalResponse::CheckpointOk => "CheckpointOk".to_string(),
        EvalResponse::Err { message } => format!("Err({message})"),
    }
}

/// Runs `f` while capturing everything written to stderr (fd 2).
///
/// Redirects fd 2 to a pipe for the duration of `f`, then restores it.
/// Returns the result of `f` alongside any lines from the captured output
/// that look like Nix warnings.
///
/// Safe to use only from the single-threaded eval-worker subprocess
/// (no Tokio runtime, no other threads). On non-Unix platforms (or if any
/// fd operation fails) warnings are silently discarded.
#[cfg(unix)]
fn capture_warnings_during<F, T>(f: F) -> (T, Vec<String>)
where
    F: FnOnce() -> T,
{
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    // SAFETY (all libc calls below): this runs on the eval-worker's single
    // thread; every fd (`2`, `saved`, `pipefd[*]`) is valid by construction and
    // failures are best-effort - on error we just skip warning capture.
    // `pipefd[0]` is handed to `File::from_raw_fd` exactly once, taking ownership.

    // Duplicate the current stderr so we can restore it later.
    let saved = unsafe { libc::dup(2) };
    if saved < 0 {
        return (f(), vec![]);
    }

    // Create a pipe: pipefd[0] = read end, pipefd[1] = write end.
    let mut pipefd = [-1i32; 2];
    if unsafe { libc::pipe(pipefd.as_mut_ptr()) } < 0 {
        unsafe { libc::close(saved) };
        return (f(), vec![]);
    }

    // Point fd 2 at the write end of the pipe and close the duplicate.
    unsafe { libc::dup2(pipefd[1], 2) };
    unsafe { libc::close(pipefd[1]) };

    // Run the evaluation. Nix writes warnings directly to fd 2.
    let result = f();

    // Restore fd 2. After this, the pipe's write end has no open fds → EOF.
    unsafe { libc::dup2(saved, 2) };
    unsafe { libc::close(saved) };

    // Read all captured output from the read end (returns at EOF).
    let mut captured = String::new();
    let mut reader = unsafe { std::fs::File::from_raw_fd(pipefd[0]) };
    let _ = reader.read_to_string(&mut captured);

    (result, parse_warnings(&captured))
}

#[cfg(not(unix))]
fn capture_warnings_during<F, T>(f: F) -> (T, Vec<String>)
where
    F: FnOnce() -> T,
{
    (f(), vec![])
}

/// Groups captured Nix stderr into whole warnings: a `warning:` line plus every
/// following line until the next log entry (`warning:`/`trace:`/`error:`/`note:`),
/// so multi-line warnings keep all their lines instead of just the first.
fn parse_warnings(captured: &str) -> Vec<String> {
    fn is_boundary(line: &str) -> bool {
        let t = line.trim_start().to_ascii_lowercase();
        ["warning:", "trace:", "error:", "note:"]
            .iter()
            .any(|p| t.starts_with(p))
    }

    let lines: Vec<&str> = captured.lines().collect();
    let mut warnings = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].to_ascii_lowercase().contains("warning:") {
            i += 1;
            continue;
        }

        let mut block = vec![lines[i].trim_end()];
        i += 1;
        while i < lines.len() && !is_boundary(lines[i]) {
            block.push(lines[i].trim_end());
            i += 1;
        }

        let joined = block.join("\n").trim().to_string();
        if !(joined.contains("SQLite database") && joined.contains("is busy")) {
            warnings.push(joined);
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_warnings_keeps_multiline_warning() {
        let captured = "warning: the following insecure packages:\n  - foo-1.2.3\nKnown issues:\n  - CVE-1234\ntrace: unrelated\n";
        let w = parse_warnings(captured);
        assert_eq!(w.len(), 1, "expected one grouped warning, got {w:?}");
        assert!(w[0].contains("insecure packages"));
        assert!(w[0].contains("foo-1.2.3"));
        assert!(w[0].contains("CVE-1234"));
        assert!(!w[0].contains("unrelated"));
    }

    #[test]
    fn parse_warnings_splits_distinct_and_drops_sqlite_busy() {
        let captured = "warning: first\nwarning: SQLite database is busy\nwarning: second\n";
        assert_eq!(
            parse_warnings(captured),
            vec!["warning: first", "warning: second"]
        );
    }
}
