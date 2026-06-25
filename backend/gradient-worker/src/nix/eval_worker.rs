/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Long-lived Nix evaluator worker subprocess.
//!
//! Parent and worker exchange rkyv length-prefixed frames (`u32 LE` + payload)
//! over stdin/stdout. `Resolve` streams one `ResolveItem` frame per attr then a
//! `ResolveEnd` terminator; all other exchanges are single-response.

use std::io::{Read, Write};
use tracing::{error, trace};

use rkyv::rancor::Error as RkyvError;
use rkyv::util::AlignedVec;

use crate::nix::nix_eval::NixEvaluator;
use crate::worker_pool::eval_stats::StatsDelta;

/// Guards against a corrupt length prefix allocating an absurd buffer. The
/// child is trusted, but a torn frame must fail fast, not OOM the parent.
pub(crate) const MAX_EVAL_FRAME: usize = 512 * 1024 * 1024;

/// Request from parent to worker. One rkyv frame per message on stdin.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug))]
pub enum EvalRequest {
    Plan { repository: String, wildcards: Vec<String> },
    List { repository: String, wildcards: Vec<String> },
    Resolve { repository: String, attrs: Vec<String> },
    Fingerprint { repository: String },
    Checkpoint { repository: String },
    Shutdown,
}

/// Response from worker to parent. One rkyv frame per message on stdout.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug))]
pub enum EvalResponse {
    PlanOk { sub_patterns: Vec<String> },
    ListOk {
        attrs: Vec<String>,
        warnings: Vec<String>,
        stats: Option<StatsDelta>,
    },
    ResolveItem { item: ResolvedItem },
    ResolveEnd {
        warnings: Vec<String>,
        stats: Option<StatsDelta>,
    },
    FingerprintOk { fingerprint: Option<String> },
    CheckpointOk,
    Err { message: String },
}

/// One resolved attr: `drv_path` set on success, `error` set on per-attr failure.
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug))]
pub struct ResolvedItem {
    pub attr: String,
    pub drv_path: Option<String>,
    pub references: Vec<String>,
    pub error: Option<String>,
}

pub(crate) fn encode_request(req: &EvalRequest) -> Result<AlignedVec, RkyvError> {
    rkyv::to_bytes::<RkyvError>(req)
}

pub(crate) fn encode_response(resp: &EvalResponse) -> Result<AlignedVec, RkyvError> {
    rkyv::to_bytes::<RkyvError>(resp)
}

pub(crate) fn decode_request(bytes: &[u8]) -> Result<EvalRequest, RkyvError> {
    let mut aligned = AlignedVec::<16>::new();
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<EvalRequest, RkyvError>(&aligned)
}

pub(crate) fn decode_response(bytes: &[u8]) -> Result<EvalResponse, RkyvError> {
    let mut aligned = AlignedVec::<16>::new();
    aligned.extend_from_slice(bytes);
    rkyv::from_bytes::<EvalResponse, RkyvError>(&aligned)
}

/// Read one `u32`-length-prefixed frame from a blocking reader. `Ok(None)` is a
/// clean EOF at a frame boundary (the parent closed the pipe).
fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_EVAL_FRAME {
        return Err(std::io::Error::other(format!("frame too large: {len}")));
    }

    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn write_frame<W: Write>(w: &mut W, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

/// Subprocess entry point. Reads `EvalRequest` frames from stdin, processes
/// them with one persistent `NixEvaluator`, writes `EvalResponse` frames to
/// stdout. Returns when stdin reaches EOF or a `Shutdown` request is received.
///
/// Diagnostics go through `tracing` (configured in `worker::main` to write
/// formatted records to stderr, which the parent inherits) so init failures
/// and stdin errors stay visible to JSON log aggregators with structured
/// fields, target metadata, and `RUST_LOG` filtering applied.
pub fn run_eval_worker() -> std::io::Result<()> {
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
    let collect_stats = crate::worker_pool::eval_stats::metrics_enabled();
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

    let mut reader = std::io::BufReader::new(std::io::stdin());
    let mut writer = std::io::BufWriter::new(std::io::stdout());

    loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(f)) => f,
            Ok(None) => return Ok(()),
            Err(e) => {
                error!(error = %e, "eval worker: stdin read error");
                return Err(e);
            }
        };

        let req: EvalRequest = match decode_request(&frame) {
            Ok(r) => r,
            Err(e) => {
                write_response(
                    &mut writer,
                    &EvalResponse::Err { message: format!("malformed request: {e}") },
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
                        &EvalResponse::Err { message: "evaluator not initialized".to_string() },
                    )?;
                    continue;
                };

                let mut all_warnings = Vec::new();
                let (walker, build_warnings) = capture_warnings_during(|| ev.walker(&repository));
                all_warnings.extend(build_warnings);

                match walker {
                    Ok(walker) => {
                        for attr in attrs {
                            let (result, warnings) =
                                capture_warnings_during(|| walker.resolve(&attr));
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
                            write_response(&mut writer, &EvalResponse::ResolveItem { item })?;
                        }

                        let _ = walker.commit_cache();
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        for attr in attrs {
                            let item = ResolvedItem {
                                attr,
                                drv_path: None,
                                references: vec![],
                                error: Some(msg.clone()),
                            };
                            write_response(&mut writer, &EvalResponse::ResolveItem { item })?;
                        }
                    }
                }

                all_warnings.dedup();
                let stats = take_delta(ev);
                write_response(
                    &mut writer,
                    &EvalResponse::ResolveEnd { warnings: all_warnings, stats },
                )?;
                continue;
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
            EvalResponse::FingerprintOk { fingerprint } => {
                format!("FingerprintOk({})", fingerprint.is_some())
            }
            EvalResponse::CheckpointOk => "CheckpointOk".to_string(),
            EvalResponse::Err { message } => format!("Err({message})"),
            EvalResponse::ResolveItem { .. } | EvalResponse::ResolveEnd { .. } => unreachable!(),
        };
        trace!(%kind, "eval worker sending response");
        write_response(&mut writer, &resp)?;
    }
}

fn write_response<W: Write>(w: &mut W, resp: &EvalResponse) -> std::io::Result<()> {
    let payload = encode_response(resp).map_err(std::io::Error::other)?;
    write_frame(w, &payload)
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
    fn eval_request_rkyv_roundtrip() {
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
            let bytes = encode_request(req).expect("encode");
            let back = decode_request(&bytes).expect("decode");
            assert_eq!(&back, req);
        }
    }

    #[test]
    fn eval_response_rkyv_roundtrip() {
        let responses: Vec<EvalResponse> = vec![
            EvalResponse::PlanOk {
                sub_patterns: vec!["packages.x86_64-linux.#".into()],
            },
            EvalResponse::ListOk {
                attrs: vec!["packages.x86_64-linux.hello".into()],
                warnings: vec![],
                stats: None,
            },
            EvalResponse::ResolveItem {
                item: ResolvedItem {
                    attr: "packages.x86_64-linux.hello".into(),
                    drv_path: Some("aaaa-hello.drv".into()),
                    references: vec![],
                    error: None,
                },
            },
            EvalResponse::ResolveEnd {
                warnings: vec!["warning: insecure".into()],
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
            let bytes = encode_response(resp).expect("encode");
            let back = decode_response(&bytes).expect("decode");
            assert_eq!(&back, resp);
        }
    }
}
