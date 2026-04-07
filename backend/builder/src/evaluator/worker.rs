/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Long-lived Nix evaluator worker subprocess.
//!
//! The parent process spawns one or more copies of the gradient binary with
//! `--eval-worker` to host a persistent [`NixEvaluator`]. Parent and worker
//! exchange line-delimited JSON over the worker's stdin/stdout, so the libnix
//! init cost is paid only once per worker (vs. once per `resolve` call when
//! using the in-process resolver).
//!
//! This file defines the wire protocol shared by both sides plus the
//! subprocess entry point [`run_eval_worker`]. The pool implementation lives
//! in [`super::worker_pool`].

use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};

use super::flake::discover_derivations;
use super::nix_commands::get_derivation_path;
use super::nix_eval::{NixEvaluator, escape_nix_str};

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
    ListOk { attrs: Vec<String> },
    ResolveOk { items: Vec<ResolvedItem> },
    AttrNamesOk { keys: Vec<String> },
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
                match discover_derivations(ev, &repository, &wildcards) {
                    Ok(attrs) => EvalResponse::ListOk { attrs },
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

                let mut items = Vec::with_capacity(attrs.len());
                for attr in attrs {
                    match get_derivation_path(ev, &repository, &attr) {
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
                EvalResponse::ResolveOk { items }
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
                match ev.attr_names(&expr) {
                    Ok(keys) => EvalResponse::AttrNamesOk { keys },
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
