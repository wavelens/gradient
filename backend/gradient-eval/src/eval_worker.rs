/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Long-lived Nix evaluator worker subprocess.
//!
//! The parent process spawns one or more copies of the gradient-worker binary
//! with `--eval-worker` to host a persistent [`NixEvaluator`]. Parent and
//! worker exchange line-delimited JSON over the worker's stdin/stdout, so the
//! libnix init cost is paid only once per worker (vs. once per `resolve` call
//! when using the in-process resolver).
//!
//! This file defines the wire protocol shared by both sides plus the
//! subprocess entry point [`run_eval_worker`]. The pool implementation lives
//! in [`super::worker_pool`].

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use tracing::{error, trace};

use crate::nix_eval::NixEvaluator;
use crate::stats::StatsDelta;

/// Request from parent → worker. One JSON object per line on the worker's stdin.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EvalRequest {
    /// Split `wildcards` into disjoint sub-patterns (one per first-wildcard
    /// child) so the parent can fan discovery across the pool, one shard per
    /// system within each worker's memory budget.
    Plan {
        repository: String,
        wildcards: Vec<String>,
    },
    /// Discover all attribute paths in `repository` matching `wildcards`.
    List {
        repository: String,
        wildcards: Vec<String>,
    },
    /// Resolve a batch of attribute paths to `(drv_path, references)` tuples.
    /// Per-attr failures are reported in the response, not as a top-level err.
    Resolve {
        repository: String,
        attrs: Vec<String>,
    },
    /// Return `repository`'s eval-cache fingerprint without evaluating it.
    /// `None` in the response for mutable/dirty flakes.
    Fingerprint { repository: String },
    /// Fold the eval-cache WAL into the main `.sqlite` (truncate checkpoint).
    /// Run once after all shards finish, before the fleet-share push.
    Checkpoint { repository: String },
    /// Ask the worker to exit cleanly. Parent uses this on graceful shutdown.
    Shutdown,
}

/// Response from worker → parent. One JSON object per line on the worker's stdout.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalResponse {
    PlanOk {
        sub_patterns: Vec<String>,
    },
    ListOk {
        attrs: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
        #[serde(default)]
        stats: Option<crate::stats::StatsDelta>,
    },
    ResolveOk {
        items: Vec<ResolvedItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
        #[serde(default)]
        stats: Option<crate::stats::StatsDelta>,
    },
    FingerprintOk {
        fingerprint: Option<String>,
    },
    CheckpointOk,
    Err {
        message: String,
    },
}

/// One element of a `ResolveOk` payload. Either `drv_path` is set (success)
/// or `error` is set (failure for that one attr).
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedItem {
    pub attr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drv_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Subprocess entry point. Reads `EvalRequest` lines from stdin, processes
/// them with one persistent `NixEvaluator`, writes `EvalResponse` lines to
/// stdout. Returns when stdin reaches EOF or a `Shutdown` request is received.
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

    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e) => {
                error!(error = %e, "eval worker: stdin read error");
                return Err(e);
            }
        };
        if n == 0 {
            // EOF: parent closed the pipe.
            return Ok(());
        }

        let req: EvalRequest = match serde_json::from_str(line.trim_end()) {
            Ok(r) => r,
            Err(e) => {
                write_response(
                    &mut writer,
                    &EvalResponse::Err {
                        message: format!("malformed request: {}", e),
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
            } => {
                let Some(ev) = evaluator.as_ref() else {
                    write_response(
                        &mut writer,
                        &EvalResponse::Err {
                            message: "evaluator not initialized".to_string(),
                        },
                    )?;
                    continue;
                };
                // Warnings from priming the prefix attrset resurface when each
                // shard re-forces it, so they are not captured here.
                let result = (|| -> anyhow::Result<Vec<String>> {
                    let walker = ev.walker(&repository)?;
                    let shards = walker.plan_shards(&wildcards)?;
                    let _ = walker.commit_cache();
                    Ok(shards)
                })();
                match result {
                    Ok(sub_patterns) => EvalResponse::PlanOk { sub_patterns },
                    Err(e) => EvalResponse::Err {
                        message: format!("{:#}", e),
                    },
                }
            }
            EvalRequest::List {
                repository,
                wildcards,
            } => {
                let Some(ev) = evaluator.as_ref() else {
                    write_response(
                        &mut writer,
                        &EvalResponse::Err {
                            message: "evaluator not initialized".to_string(),
                        },
                    )?;
                    continue;
                };
                let (result, warnings) =
                    capture_warnings_during(|| -> anyhow::Result<Vec<String>> {
                        let walker = ev.walker(&repository)?;
                        let attrs = walker.discover(&wildcards)?;
                        let _ = walker.commit_cache();
                        Ok(attrs)
                    });
                let stats = take_delta(ev);
                match result {
                    Ok(attrs) => EvalResponse::ListOk {
                        attrs,
                        warnings,
                        stats,
                    },
                    Err(e) => EvalResponse::Err {
                        message: format!("{:#}", e),
                    },
                }
            }
            EvalRequest::Resolve { repository, attrs } => {
                let Some(ev) = evaluator.as_ref() else {
                    write_response(
                        &mut writer,
                        &EvalResponse::Err {
                            message: "evaluator not initialized".to_string(),
                        },
                    )?;
                    continue;
                };

                let mut all_warnings = Vec::new();
                let mut items = Vec::with_capacity(attrs.len());
                let (walker, build_warnings) = capture_warnings_during(|| ev.walker(&repository));
                all_warnings.extend(build_warnings);

                match walker {
                    Ok(walker) => {
                        for attr in attrs {
                            let (result, warnings) =
                                capture_warnings_during(|| walker.resolve(&attr));
                            all_warnings.extend(warnings);
                            match result {
                                Ok((drv, references)) => items.push(ResolvedItem {
                                    attr,
                                    drv_path: Some(drv),
                                    references,
                                    error: None,
                                }),
                                Err(e) => items.push(ResolvedItem {
                                    attr,
                                    drv_path: None,
                                    references: vec![],
                                    error: Some(format!("{:#}", e)),
                                }),
                            }
                        }

                        let _ = walker.commit_cache();
                    }
                    Err(e) => {
                        let msg = format!("{:#}", e);
                        for attr in attrs {
                            items.push(ResolvedItem {
                                attr,
                                drv_path: None,
                                references: vec![],
                                error: Some(msg.clone()),
                            });
                        }
                    }
                }

                all_warnings.dedup();
                let stats = take_delta(ev);
                EvalResponse::ResolveOk {
                    items,
                    warnings: all_warnings,
                    stats,
                }
            }
            EvalRequest::Fingerprint { repository } => {
                let Some(ev) = evaluator.as_ref() else {
                    write_response(
                        &mut writer,
                        &EvalResponse::Err {
                            message: "evaluator not initialized".to_string(),
                        },
                    )?;
                    continue;
                };
                match ev.fingerprint(&repository) {
                    Ok(fingerprint) => EvalResponse::FingerprintOk { fingerprint },
                    Err(e) => EvalResponse::Err {
                        message: format!("{:#}", e),
                    },
                }
            }
            EvalRequest::Checkpoint { repository } => {
                let Some(ev) = evaluator.as_ref() else {
                    write_response(
                        &mut writer,
                        &EvalResponse::Err {
                            message: "evaluator not initialized".to_string(),
                        },
                    )?;
                    continue;
                };
                let result =
                    (|| -> anyhow::Result<()> { ev.walker(&repository)?.checkpoint_cache() })();
                match result {
                    Ok(()) => EvalResponse::CheckpointOk,
                    Err(e) => EvalResponse::Err {
                        message: format!("{:#}", e),
                    },
                }
            }
        };

        let kind = match &resp {
            EvalResponse::PlanOk { sub_patterns } => {
                format!("PlanOk({} shards)", sub_patterns.len())
            }
            EvalResponse::ListOk { attrs, .. } => format!("ListOk({} attrs)", attrs.len()),
            EvalResponse::ResolveOk { items, .. } => format!("ResolveOk({} items)", items.len()),
            EvalResponse::FingerprintOk { fingerprint } => {
                format!("FingerprintOk({})", fingerprint.is_some())
            }
            EvalResponse::CheckpointOk => "CheckpointOk".to_string(),
            EvalResponse::Err { message } => format!("Err({message})"),
        };
        trace!(%kind, "eval worker sending response");
        write_response(&mut writer, &resp)?;
    }
}

fn write_response<W: Write>(w: &mut W, resp: &EvalResponse) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(resp).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    w.write_all(&bytes)?;
    w.flush()
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

    #[test]
    fn eval_request_serde_roundtrip() {
        let requests = [
            EvalRequest::Plan {
                repository: "github:nixos/nixpkgs".into(),
                wildcards: vec!["packages.*.*".into()],
            },
            EvalRequest::List {
                repository: "github:nixos/nixpkgs".into(),
                wildcards: vec!["packages.*.*".into()],
            },
            EvalRequest::Resolve {
                repository: "github:nixos/nixpkgs".into(),
                attrs: vec!["packages.x86_64-linux.hello".into()],
            },
            EvalRequest::Fingerprint {
                repository: "github:nixos/nixpkgs".into(),
            },
            EvalRequest::Checkpoint {
                repository: "github:nixos/nixpkgs".into(),
            },
            EvalRequest::Shutdown,
        ];

        for req in &requests {
            let json = serde_json::to_string(req).expect("serialize failed");
            let back: EvalRequest = serde_json::from_str(&json).expect("deserialize failed");
            // Re-serialize and compare JSON strings (no PartialEq on enums)
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                json,
                "roundtrip mismatch for request"
            );
        }
    }

    #[test]
    fn eval_response_serde_roundtrip() {
        let responses: Vec<EvalResponse> = vec![
            EvalResponse::PlanOk {
                sub_patterns: vec!["packages.x86_64-linux.#".into()],
            },
            EvalResponse::ListOk {
                attrs: vec!["packages.x86_64-linux.hello".into()],
                warnings: vec![],
                stats: None,
            },
            EvalResponse::ResolveOk {
                items: vec![ResolvedItem {
                    attr: "packages.x86_64-linux.hello".into(),
                    drv_path: Some("aaaa-hello.drv".into()),
                    references: vec![],
                    error: None,
                }],
                warnings: vec![],
                stats: None,
            },
            EvalResponse::FingerprintOk {
                fingerprint: Some("deadbeef".into()),
            },
            EvalResponse::CheckpointOk,
            EvalResponse::Err {
                message: "something went wrong".into(),
            },
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp).expect("serialize failed");
            let back: EvalResponse = serde_json::from_str(&json).expect("deserialize failed");
            assert_eq!(
                serde_json::to_string(&back).unwrap(),
                json,
                "roundtrip mismatch for response"
            );
        }
    }
}
