/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::Write;
use tracing::{error, trace};

use crate::flake_walk::FlakeWalker;
use crate::ipc::{
    EVAL_IPC_VERSION, EvalRequest, EvalResponse, ResolvedItem, decode_request, encode_response,
    read_frame, write_frame,
};
use crate::nix_eval::NixEvaluator;
use crate::stats::StatsDelta;

pub fn run_eval_worker() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    writer.write_all(&[EVAL_IPC_VERSION])?;
    writer.flush()?;

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

    let collect_stats = crate::stats::metrics_enabled();
    let mut last = if collect_stats {
        evaluator
            .as_ref()
            .and_then(|ev| ev.stats().ok())
            .unwrap_or_default()
    } else {
        nix_bindings::EvalStats::default()
    };

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

    let mut walkers = WalkerCache { entry: None };

    loop {
        let Some(payload) = read_frame(&mut reader).inspect_err(|e| {
            error!(error = %e, "eval worker: stdin read error");
        })?
        else {
            return Ok(());
        };

        let req = match decode_request(&payload) {
            Ok(r) => r,
            Err(e) => {
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
                input_overrides,
            } => with_evaluator(&evaluator, |ev| {
                // Warnings from priming the prefix attrset are resurfacing in every shard.
                // Per-attr eval errors are captured here, because a thrown shard root
                // is leaving no shard for a later `List` to re-hit.
                or_err(
                    walkers
                        .with(ev, &repository, &input_overrides, |walker| {
                            let (shards, errors) = walker.plan_shards(&wildcards)?;
                            let _ = walker.commit_cache();
                            Ok((shards, errors))
                        })
                        .map(|(shards, errors)| EvalResponse::PlanOk { shards, errors }),
                )
            }),
            EvalRequest::List {
                repository,
                wildcards,
                only,
                input_overrides,
            } => with_evaluator(&evaluator, |ev| {
                let (result, warnings) = capture_warnings_during(|| {
                    walkers.with(ev, &repository, &input_overrides, |walker| {
                        let listing = walker.discover_split(&wildcards, only.as_deref())?;
                        let _ = walker.commit_cache();
                        Ok(listing)
                    })
                });
                let stats = take_delta(ev);
                or_err(
                    result.map(|(attrs, deferred, errors)| EvalResponse::ListOk {
                        attrs,
                        deferred,
                        warnings,
                        errors,
                        stats,
                    }),
                )
            }),
            EvalRequest::Resolve {
                repository,
                attrs,
                input_overrides,
            } => {
                let resp = match evaluator.as_ref() {
                    None => EvalResponse::Err {
                        message: "evaluator not initialized".to_string(),
                    },
                    Some(ev) => {
                        let (warnings, io) = stream_resolve(
                            &mut writer,
                            ev,
                            &mut walkers,
                            &repository,
                            &input_overrides,
                            attrs,
                        );
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
            EvalRequest::Fingerprint {
                repository,
                input_overrides,
            } => with_evaluator(&evaluator, |ev| {
                or_err(
                    ev.fingerprint(&repository, &input_overrides)
                        .map(|fingerprint| EvalResponse::FingerprintOk { fingerprint }),
                )
            }),
            EvalRequest::Checkpoint {
                repository,
                input_overrides,
            } => with_evaluator(&evaluator, |ev| {
                or_err(
                    walkers
                        .with(ev, &repository, &input_overrides, |walker| {
                            walker.checkpoint_cache()
                        })
                        .map(|()| EvalResponse::CheckpointOk),
                )
            }),
        };

        trace!(kind = response_kind(&resp), "eval worker sending response");
        send(&mut writer, &resp)?;
    }
}

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

fn or_err(result: anyhow::Result<EvalResponse>) -> EvalResponse {
    result.unwrap_or_else(|e| EvalResponse::Err {
        message: format!("{e:#}"),
    })
}

/// The cache key is including the input overrides.
/// A pooled worker must never serve a stale locked flake for a new override set.
type CachedWalker<'ev> = (String, Vec<(String, String)>, FlakeWalker<'ev>);

struct WalkerCache<'ev> {
    entry: Option<CachedWalker<'ev>>,
}

impl<'ev> WalkerCache<'ev> {
    fn open(
        &mut self,
        ev: &'ev NixEvaluator,
        repository: &str,
        overrides: &[(String, String)],
    ) -> anyhow::Result<&FlakeWalker<'ev>> {
        let stale = self
            .entry
            .as_ref()
            .is_none_or(|(repo, ovr, _)| repo != repository || ovr.as_slice() != overrides);
        if stale {
            self.entry = None;
            let walker = ev.walker(repository, overrides)?;
            self.entry = Some((repository.to_string(), overrides.to_vec(), walker));
        }

        Ok(&self.entry.as_ref().expect("entry just ensured").2)
    }

    fn with<T>(
        &mut self,
        ev: &'ev NixEvaluator,
        repository: &str,
        overrides: &[(String, String)],
        f: impl FnOnce(&FlakeWalker<'ev>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        f(self.open(ev, repository, overrides)?)
    }
}

fn stream_resolve<'ev, W: Write>(
    writer: &mut W,
    ev: &'ev NixEvaluator,
    walkers: &mut WalkerCache<'ev>,
    repository: &str,
    overrides: &[(String, String)],
    attrs: Vec<String>,
) -> (Vec<String>, std::io::Result<()>) {
    let mut all_warnings = Vec::new();
    let mut io = Ok(());
    let emit = |writer: &mut W, io: &mut std::io::Result<()>, item: ResolvedItem| {
        if io.is_ok() {
            *io = send(writer, &EvalResponse::ResolveItem { item });
        }
    };

    let (walker_result, build_warnings) =
        capture_warnings_during(|| walkers.open(ev, repository, overrides));
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
        EvalResponse::PlanOk { shards, .. } => format!("PlanOk({} shards)", shards.len()),
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

#[cfg(unix)]
fn capture_warnings_during<F, T>(f: F) -> (T, Vec<String>)
where
    F: FnOnce() -> T,
{
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    // SAFETY (all libc calls below): this code is executing on the eval-worker's single thread.
    // Every fd (`2`, `saved`, `pipefd[*]`) is valid by construction.
    // Failures are best-effort and are only skipping the warning capture.
    // `pipefd[0]` is handed to `File::from_raw_fd` exactly once, taking ownership.

    let saved = unsafe { libc::dup(2) };
    if saved < 0 {
        return (f(), vec![]);
    }

    let mut pipefd = [-1i32; 2];
    if unsafe { libc::pipe(pipefd.as_mut_ptr()) } < 0 {
        unsafe { libc::close(saved) };
        return (f(), vec![]);
    }

    unsafe { libc::dup2(pipefd[1], 2) };
    unsafe { libc::close(pipefd[1]) };

    let result = f();

    unsafe { libc::dup2(saved, 2) };
    unsafe { libc::close(saved) };

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
