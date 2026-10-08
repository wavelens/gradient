/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read, Write};
use tracing::{error, trace};

use crate::flake_walk::FlakeWalker;
use crate::frames::Frames;
use crate::ipc::{
    EVAL_IPC_VERSION, EvalRequest, EvalResponse, ResolvedItem, decode_request, read_frame,
};
use crate::nix_eval::{NixEvaluator, StatsReader};

pub fn run_eval_worker() -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    stdout.write_all(&[EVAL_IPC_VERSION])?;
    stdout.flush()?;

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
    let last = evaluator
        .as_ref()
        .and_then(|ev| read_stats(ev, collect_stats))
        .unwrap_or_default();
    let frames = &Frames::new(stdout, last);
    let (stop, stopped) = std::sync::mpsc::channel::<()>();

    std::thread::scope(|scope| {
        if let Some(reader) = evaluator
            .as_ref()
            .filter(|_| collect_stats)
            .map(NixEvaluator::stats_reader)
        {
            scope.spawn(move || tick_stats(frames, reader, &stopped));
        }
        let served = serve(
            &mut std::io::stdin().lock(),
            frames,
            &evaluator,
            collect_stats,
        );
        drop(stop);
        served
    })
}

const STATS_TICK: std::time::Duration = std::time::Duration::from_secs(1);

fn tick_stats<W: Write>(
    frames: &Frames<W>,
    reader: StatsReader<'_>,
    stopped: &std::sync::mpsc::Receiver<()>,
) {
    let Ok(ctx) = nix_bindings::Context::new_no_load_config() else {
        return;
    };
    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(STATS_TICK) {
        if let Ok(now) = reader.read(&ctx)
            && frames.tick(now).is_err()
        {
            return;
        }
    }
}

fn read_stats(ev: &NixEvaluator, collect_stats: bool) -> Option<nix_bindings::EvalStats> {
    collect_stats.then(|| ev.stats().ok()).flatten()
}

fn serve<W: Write>(
    reader: &mut impl Read,
    frames: &Frames<W>,
    evaluator: &Option<NixEvaluator>,
    collect_stats: bool,
) -> std::io::Result<()> {
    let mut walkers = WalkerCache { entry: None };

    loop {
        let Some(payload) = read_frame(reader).inspect_err(|e| {
            error!(error = %e, "eval worker: stdin read error");
        })?
        else {
            return Ok(());
        };

        let req = match decode_request(&payload) {
            Ok(r) => r,
            Err(e) => {
                frames.send(&EvalResponse::Err {
                    message: format!("malformed request: {e}"),
                })?;
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
            } => with_evaluator(evaluator, |ev| {
                // Warnings from priming the prefix attrset are resurfacing in every shard.
                // Per-attr eval errors are captured here, because a thrown shard root
                // is leaving no shard for a later `List` to re-hit.
                frames.begin();
                let planned = walkers.with(ev, &repository, &input_overrides, |walker| {
                    walker.plan_shards(&wildcards)
                });
                frames.end(None);
                or_err(planned.map(|(shards, errors)| EvalResponse::PlanOk { shards, errors }))
            }),
            EvalRequest::List {
                repository,
                wildcards,
                only,
                input_overrides,
            } => with_evaluator(evaluator, |ev| {
                frames.begin();
                let (result, warnings) = capture_warnings_during(|| {
                    walkers.with(ev, &repository, &input_overrides, |walker| {
                        walker.discover_split(&wildcards, only.as_deref())
                    })
                });
                let stats = frames.end(read_stats(ev, collect_stats));
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
                        frames.begin();
                        let (warnings, io) = stream_resolve(
                            frames,
                            ev,
                            &mut walkers,
                            &repository,
                            &input_overrides,
                            attrs,
                        );
                        let stats = frames.end(read_stats(ev, collect_stats));
                        io?;
                        EvalResponse::ResolveEnd { warnings, stats }
                    }
                };
                frames.send(&resp)?;
                continue;
            }
            EvalRequest::FetchInput {
                locked,
                git_ssh_command,
            } => with_evaluator(evaluator, |ev| {
                or_err(
                    ev.fetch_tree(&locked, git_ssh_command.as_deref())
                        .map(|store_path| EvalResponse::FetchOk { store_path }),
                )
            }),
            EvalRequest::Fingerprint {
                repository,
                input_overrides,
            } => with_evaluator(evaluator, |ev| {
                or_err(
                    ev.fingerprint(&repository, &input_overrides)
                        .map(|fingerprint| EvalResponse::FingerprintOk { fingerprint }),
                )
            }),
            EvalRequest::Checkpoint {
                repository,
                input_overrides,
            } => with_evaluator(evaluator, |ev| {
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
        frames.send(&resp)?;
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
    frames: &Frames<W>,
    ev: &'ev NixEvaluator,
    walkers: &mut WalkerCache<'ev>,
    repository: &str,
    overrides: &[(String, String)],
    attrs: Vec<String>,
) -> (Vec<String>, std::io::Result<()>) {
    let mut all_warnings = Vec::new();
    let mut io = Ok(());
    let emit = |io: &mut std::io::Result<()>, item: ResolvedItem| {
        if io.is_ok() {
            *io = frames.send(&EvalResponse::ResolveItem { item });
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
                emit(&mut io, item);
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            for attr in attrs {
                emit(
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
        EvalResponse::FetchOk { store_path } => format!("FetchOk({store_path})"),
        EvalResponse::Stats { delta } => format!("Stats({} thunks)", delta.nr_thunks),
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

    // The pipe is drained while `f` runs. Output beyond the pipe buffer would otherwise block
    // the evaluating thread for good, with nobody left to read it.
    let mut reader = unsafe { std::fs::File::from_raw_fd(pipefd[0]) };
    let drain = std::thread::spawn(move || {
        let mut captured = String::new();
        let _ = reader.read_to_string(&mut captured);
        captured
    });

    let result = f();

    unsafe { libc::dup2(saved, 2) };
    unsafe { libc::close(saved) };

    let captured = drain.join().unwrap_or_default();

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

        warnings.push(block.join("\n").trim().to_string());
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
    fn parse_warnings_splits_distinct_warnings() {
        let captured =
            "warning: first\nerror (ignored): SQLite database is busy\nwarning: second\n";
        assert_eq!(
            parse_warnings(captured),
            vec![
                "warning: first\nerror (ignored): SQLite database is busy",
                "warning: second"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_keeps_draining_past_the_pipe_buffer() {
        use std::io::Write;

        let line = format!("warning: {}\n", "x".repeat(1023));
        let (result, warnings) = capture_warnings_during(|| {
            let mut stderr = std::io::stderr().lock();
            for _ in 0..256 {
                stderr.write_all(line.as_bytes()).expect("stderr write");
            }
            7
        });

        assert_eq!(result, 7);
        assert_eq!(warnings.len(), 256);
    }
}
