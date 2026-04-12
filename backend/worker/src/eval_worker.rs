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

use crate::flake::{discover_derivations, get_derivation_path};
use crate::nix_eval::{NixEvaluator, escape_nix_str};

/// Request from parent → worker. One JSON object per line on the worker's stdin.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EvalRequest {
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
    /// Query the attribute names of a single path inside a flake. Used by the
    /// parent to fan out wildcard expansion across workers in parallel.
    AttrNames { repository: String, path: String },
    /// Ask the worker to exit cleanly. Parent uses this on graceful shutdown.
    Shutdown,
}

/// Response from worker → parent. One JSON object per line on the worker's stdout.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalResponse {
    ListOk {
        attrs: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    ResolveOk {
        items: Vec<ResolvedItem>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    AttrNamesOk {
        keys: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<String>,
    },
    Err { message: String },
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
/// Logs go to stderr (inherited from parent) so the parent's tracing layer
/// still captures them.
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
            eprintln!("eval worker: NixEvaluator init failed: {:#}", e);
            None
        }
    };

    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("eval worker: stdin read error: {}", e);
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

        let resp = match req {
            EvalRequest::Shutdown => return Ok(()),
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
                    capture_warnings_during(|| discover_derivations(ev, &repository, &wildcards));
                match result {
                    Ok(attrs) => EvalResponse::ListOk { attrs, warnings },
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
                for attr in attrs {
                    let (result, warnings) = capture_warnings_during(|| {
                        get_derivation_path(ev, &repository, &attr)
                    });
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
                all_warnings.dedup();
                EvalResponse::ResolveOk {
                    items,
                    warnings: all_warnings,
                }
            }
            EvalRequest::AttrNames { repository, path } => {
                let Some(ev) = evaluator.as_ref() else {
                    write_response(
                        &mut writer,
                        &EvalResponse::Err {
                            message: "evaluator not initialized".to_string(),
                        },
                    )?;
                    continue;
                };
                let expr = if path.is_empty() {
                    format!("(builtins.getFlake \"{}\")", escape_nix_str(&repository))
                } else {
                    format!(
                        "(builtins.getFlake \"{}\").{}",
                        escape_nix_str(&repository),
                        path
                    )
                };
                let (result, warnings) = capture_warnings_during(|| ev.attr_names(&expr));
                match result {
                    Ok(keys) => EvalResponse::AttrNamesOk { keys, warnings },
                    Err(e) => EvalResponse::Err {
                        message: format!("{:#}", e),
                    },
                }
            }
        };

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

    let warnings: Vec<String> = captured
        .lines()
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains("warning:")
        })
        .map(|l| l.trim().to_string())
        .collect();

    (result, warnings)
}

#[cfg(not(unix))]
fn capture_warnings_during<F, T>(f: F) -> (T, Vec<String>)
where
    F: FnOnce() -> T,
{
    (f(), vec![])
}
