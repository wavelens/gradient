/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tracing::{error, trace};

use crate::flake_walk::{FlakeWalker, Resolved};
use crate::frames::Frames;
use crate::ipc::{
    EVAL_IPC_VERSION, EvalRequest, EvalResponse, ResolvedItem, decode_request, read_frame,
};
use crate::nix_eval::{FetchHook, NixEvaluator, RealiseHook, StatsReader};
use crate::stats::{AttrCost, StatsDelta};

pub(crate) struct ParentPipe<R, W> {
    reader: Mutex<R>,
    frames: Frames<W>,
    broken: AtomicBool,
}

impl<R: Read, W: Write> ParentPipe<R, W> {
    pub(crate) fn new(reader: R, frames: Frames<W>) -> Self {
        Self {
            reader: Mutex::new(reader),
            frames,
            broken: AtomicBool::new(false),
        }
    }

    pub(crate) fn read_frame(&self) -> std::io::Result<Option<Vec<u8>>> {
        if self.broken.load(Ordering::Acquire) {
            return Err(std::io::Error::other(
                "eval pipe broke during a request to the parent",
            ));
        }
        read_frame(&mut *self.reader.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn request_build(&self, derived_paths: &[String]) -> Result<(), String> {
        let need = EvalResponse::NeedsBuild {
            derived_paths: derived_paths.to_vec(),
        };
        match self.ask(&need, "a build")? {
            EvalRequest::BuildDone { error: None } => Ok(()),
            EvalRequest::BuildDone { error: Some(error) } => Err(error),
            other => Err(self.break_on(Some(other), "a build")),
        }
    }

    pub(crate) fn request_input(&self, request: &str) -> Result<String, String> {
        let need = EvalResponse::NeedsInput {
            request: request.to_owned(),
        };
        match self.ask(&need, "an input")? {
            EvalRequest::InputDone {
                response: Some(response),
                error: None,
            } => Ok(response),
            EvalRequest::InputDone {
                error: Some(error), ..
            } => Err(error),
            other => Err(self.break_on(Some(other), "an input")),
        }
    }

    fn ask(&self, need: &EvalResponse, what: &str) -> Result<EvalRequest, String> {
        self.frames
            .send(need)
            .map_err(|e| format!("eval pipe closed while asking for {what}: {e}"))?;

        let answer = match self.read_frame() {
            Ok(Some(payload)) => decode_request(&payload).ok(),
            Ok(None) | Err(_) => None,
        };
        answer.ok_or_else(|| self.break_on(None, what))
    }

    fn break_on(&self, answer: Option<EvalRequest>, what: &str) -> String {
        self.broken.store(true, Ordering::Release);
        error!(?answer, "eval worker: expected the answer for {what}");
        format!("eval pipe closed while waiting for {what}")
    }
}

pub fn run_eval_worker(collect_stats: bool) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    stdout.write_all(&[EVAL_IPC_VERSION])?;
    stdout.flush()?;
    let mut fetch_through_parent = [0u8; 1];
    std::io::stdin().read_exact(&mut fetch_through_parent)?;
    let fetch_through_parent = fetch_through_parent[0] == 1;

    let pipe = Arc::new(ParentPipe::new(
        std::io::stdin(),
        Frames::new(stdout, nix_bindings::EvalStats::default()),
    ));
    let fetch_hook = fetch_through_parent.then(|| fetch_through(Arc::clone(&pipe)));
    let evaluator = match NixEvaluator::new(Some(realise_through(Arc::clone(&pipe))), fetch_hook) {
        Ok(e) => Some(e),
        Err(e) => {
            error!(
                error = format!("{e:#}"),
                "eval worker: NixEvaluator init failed"
            );
            None
        }
    };
    if let Some(last) = evaluator
        .as_ref()
        .and_then(|ev| read_stats(ev, collect_stats))
    {
        pipe.frames.set_baseline(last);
    }
    let pipe = &*pipe;
    let (stop, stopped) = std::sync::mpsc::channel::<()>();

    std::thread::scope(|scope| {
        if let Some(reader) = evaluator
            .as_ref()
            .filter(|_| collect_stats)
            .map(NixEvaluator::stats_reader)
        {
            scope.spawn(move || tick_stats(&pipe.frames, reader, &stopped));
        }
        let answered = answer_requests(pipe, &evaluator, collect_stats);
        drop(stop);
        answered
    })
}

fn realise_through<R, W>(pipe: Arc<ParentPipe<R, W>>) -> RealiseHook
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    Box::new(move |derived_paths| pipe.request_build(derived_paths))
}

fn fetch_through<R, W>(pipe: Arc<ParentPipe<R, W>>) -> FetchHook
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    Box::new(move |request| pipe.request_input(request))
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

fn answer_requests<R: Read, W: Write>(
    pipe: &ParentPipe<R, W>,
    evaluator: &Option<NixEvaluator>,
    collect_stats: bool,
) -> std::io::Result<()> {
    let frames = &pipe.frames;
    let mut walkers = WalkerCache { entry: None };

    loop {
        let Some(payload) = pipe.read_frame().inspect_err(|e| {
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
            EvalRequest::BuildDone { .. } | EvalRequest::InputDone { .. } => EvalResponse::Err {
                message: "an answer arrived outside a request to the parent".to_string(),
            },
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
                // The derivation path is resolved where the attribute was forced, so a member
                // is evaluated in the same subprocess.
                let (result, warnings) = capture_warnings_during(|| {
                    walkers.with(ev, &repository, &input_overrides, |walker| {
                        let (attrs, deferred, errors) =
                            walker.discover_split(&wildcards, only.as_deref())?;
                        let items = attrs
                            .into_iter()
                            .map(|attr| {
                                measured(
                                    || read_stats(ev, collect_stats),
                                    || resolve_item(walker, attr),
                                )
                            })
                            .collect();
                        Ok((items, deferred, errors))
                    })
                });
                let stats = frames.end(read_stats(ev, collect_stats));
                or_err(
                    result.map(|(items, deferred, errors)| EvalResponse::ListOk {
                        items,
                        deferred,
                        warnings,
                        errors,
                        stats,
                    }),
                )
            }),
            EvalRequest::FetchInput {
                request,
                git_ssh_command,
            } => with_evaluator(evaluator, |ev| {
                or_err(
                    ev.fetch_tree(&request, git_ssh_command.as_deref())
                        .map(|response| EvalResponse::FetchOk { response }),
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
/// A pooled worker must never hand out a stale locked flake for a new override set.
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

fn measured(
    read_stats: impl Fn() -> Option<nix_bindings::EvalStats>,
    resolve: impl FnOnce() -> ResolvedItem,
) -> ResolvedItem {
    let before = read_stats();
    let started = Instant::now();
    let mut item = resolve();
    let eval_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    item.cost = before.zip(read_stats()).map(|(before, after)| AttrCost {
        stats: StatsDelta::between(&after, &before),
        eval_ms,
    });
    item
}

fn resolve_item(walker: &FlakeWalker<'_>, attr: String) -> ResolvedItem {
    match walker.resolve(&attr) {
        Ok(Resolved {
            drv_path,
            licenses,
            runner,
        }) => ResolvedItem {
            attr,
            drv_path: Some(drv_path),
            licenses,
            error: None,
            cost: None,
            runner,
        },
        Err(e) => ResolvedItem {
            attr,
            drv_path: None,
            licenses: vec![],
            error: Some(format!("{e:#}")),
            cost: None,
            runner: None,
        },
    }
}

fn response_kind(resp: &EvalResponse) -> String {
    match resp {
        EvalResponse::PlanOk { shards, .. } => format!("PlanOk({} shards)", shards.len()),
        EvalResponse::ListOk { items, .. } => format!("ListOk({} items)", items.len()),
        EvalResponse::FingerprintOk { fingerprint } => {
            format!("FingerprintOk({})", fingerprint.is_some())
        }
        EvalResponse::FetchOk { response } => format!("FetchOk({response})"),
        EvalResponse::Stats { delta } => format!("Stats({} thunks)", delta.nr_thunks),
        EvalResponse::NeedsBuild { derived_paths } => {
            format!("NeedsBuild({} paths)", derived_paths.len())
        }
        EvalResponse::NeedsInput { request } => format!("NeedsInput({request})"),
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
    fn starts_a_block(line: &str) -> bool {
        let t = line.trim_start().to_ascii_lowercase();
        t.contains("warning:")
            || ["trace:", "error:", "note:"]
                .iter()
                .any(|p| t.starts_with(p))
    }

    let lines: Vec<&str> = captured
        .lines()
        .filter(|line| !is_ignored_line(line))
        .map(str::trim_end)
        .collect();

    let mut warnings = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].to_ascii_lowercase().contains("warning:") {
            i += 1;
            continue;
        }

        let start = i;
        i += 1;
        while i < lines.len() && !starts_a_block(lines[i]) {
            i += 1;
        }

        let block = lines[start..i].join("\n").trim().to_string();
        if !warnings.contains(&block) {
            warnings.push(block);
        }
    }
    warnings
}

fn is_ignored_line(line: &str) -> bool {
    let t = line.to_ascii_lowercase();
    t.contains("sqlite database") && t.contains("is busy")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{decode_response, encode_request, write_frame};
    use std::io::Cursor;

    fn queued(requests: &[EvalRequest]) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        for req in requests {
            write_frame(&mut bytes, &encode_request(req).unwrap()).unwrap();
        }
        Cursor::new(bytes)
    }

    fn pipe_over(requests: &[EvalRequest]) -> ParentPipe<Cursor<Vec<u8>>, Vec<u8>> {
        ParentPipe::new(
            queued(requests),
            Frames::new(Vec::new(), nix_bindings::EvalStats::default()),
        )
    }

    fn sent(pipe: ParentPipe<Cursor<Vec<u8>>, Vec<u8>>) -> Vec<EvalResponse> {
        let mut cursor = Cursor::new(pipe.frames.into_output());
        std::iter::from_fn(|| read_frame(&mut cursor).unwrap())
            .map(|f| decode_response(&f).unwrap())
            .collect()
    }

    #[test]
    fn a_build_request_round_trips_through_the_link() {
        let pipe = pipe_over(&[EvalRequest::BuildDone {
            error: Some("boom".into()),
        }]);

        let built = pipe.request_build(&["/nix/store/aaaa-src.drv^out".to_string()]);

        assert_eq!(built, Err("boom".to_string()));
        let sent = sent(pipe);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            matches!(&sent[0], EvalResponse::NeedsBuild { derived_paths }
                if derived_paths == &["/nix/store/aaaa-src.drv^out".to_string()]),
            "{sent:?}"
        );
    }

    #[test]
    fn a_finished_build_lets_the_evaluation_go_on() {
        let pipe = pipe_over(&[
            EvalRequest::BuildDone { error: None },
            EvalRequest::Shutdown,
        ]);

        assert_eq!(
            pipe.request_build(&["/nix/store/aaaa-src.drv^out".into()]),
            Ok(())
        );
        let next = pipe.read_frame().unwrap().expect("the next request");
        assert!(matches!(
            decode_request(&next).unwrap(),
            EvalRequest::Shutdown
        ));
    }

    #[test]
    fn an_unexpected_frame_during_a_build_wait_is_an_error() {
        let pipe = pipe_over(&[EvalRequest::Shutdown, EvalRequest::Shutdown]);

        let built = pipe.request_build(&["/nix/store/aaaa-src.drv^out".into()]);

        assert!(built.is_err(), "{built:?}");
        assert!(
            pipe.read_frame().is_err(),
            "a broken pipe ends the request loop"
        );
    }

    #[test]
    fn a_closed_link_during_a_build_wait_is_an_error() {
        let pipe = pipe_over(&[]);

        assert_eq!(
            pipe.request_build(&["/nix/store/aaaa-src.drv^out".into()]),
            Err("eval pipe closed while waiting for a build".to_string())
        );
    }

    #[test]
    fn request_input_returns_the_parent_answer() {
        let request = r#"{"builtin":"fetchTree","args":{"type":"tarball","url":"https://x"}}"#;
        let pipe = pipe_over(&[EvalRequest::InputDone {
            response: Some(r#"{"storePath":"/nix/store/aaaa-source"}"#.into()),
            error: None,
        }]);

        let fetched = pipe.request_input(request);

        assert_eq!(
            fetched,
            Ok(r#"{"storePath":"/nix/store/aaaa-source"}"#.to_string())
        );
        let sent = sent(pipe);
        assert!(
            matches!(&sent[..], [EvalResponse::NeedsInput { request: r }] if r == request),
            "{sent:?}"
        );
    }

    #[test]
    fn a_wrong_answer_to_an_input_request_breaks_the_pipe() {
        let pipe = pipe_over(&[
            EvalRequest::BuildDone { error: None },
            EvalRequest::Shutdown,
        ]);

        assert_eq!(
            pipe.request_input("{}"),
            Err("eval pipe closed while waiting for an input".to_string())
        );
        assert!(
            pipe.read_frame().is_err(),
            "a broken pipe ends the request loop"
        );
    }

    fn unresolved(attr: &str) -> ResolvedItem {
        ResolvedItem {
            attr: attr.into(),
            drv_path: None,
            licenses: vec![],
            error: None,
            cost: None,
            runner: None,
        }
    }

    #[test]
    fn an_attribute_is_charged_the_counters_spent_resolving_it() {
        let counter = std::cell::Cell::new(100);
        let read = || {
            Some(nix_bindings::EvalStats {
                nr_thunks: counter.get(),
                ..Default::default()
            })
        };

        let item = measured(read, || {
            counter.set(160);
            unresolved("packages.x86_64-linux.hello")
        });

        assert_eq!(item.cost.map(|c| c.stats.nr_thunks), Some(60));
    }

    #[test]
    fn parse_warnings_keeps_multiline_warning() {
        let captured = "warning: the following insecure packages:\n  - foo-1.2.3\nKnown issues:\n  - CVE-1234\ntrace: unrelated\n";
        let w = parse_warnings(captured);
        assert_eq!(w.len(), 1, "a single grouped warning: {w:?}");
        assert!(w[0].contains("insecure packages"));
        assert!(w[0].contains("foo-1.2.3"));
        assert!(w[0].contains("CVE-1234"));
        assert!(!w[0].contains("unrelated"));
    }

    #[test]
    fn parse_warnings_splits_distinct_warnings() {
        let captured = "warning: first\nerror (ignored): ignored mid-block\nwarning: second\n";
        assert_eq!(
            parse_warnings(captured),
            vec![
                "warning: first\nerror (ignored): ignored mid-block",
                "warning: second"
            ]
        );
    }

    #[test]
    fn a_busy_eval_cache_line_is_dropped() {
        let captured = "warning: first\n\
             error (ignored): SQLite database \"/root/eval-cache/a.sqlite\" is busy\n\
             error (ignored): SQLite database \"/root/eval-cache/b.sqlite\" is busy\n";
        assert_eq!(parse_warnings(captured), vec!["warning: first"]);
    }

    #[test]
    fn a_repeated_warning_collapses() {
        let captured = "warning: same\nwarning: same\nwarning: same\ntrace: nix callsite\n";
        assert_eq!(parse_warnings(captured), vec!["warning: same"]);
    }

    #[test]
    fn a_prefixed_warning_repeated_per_shard_collapses() {
        let head = "evaluation warning: the legacy table is outdated";
        let captured = format!("{head}\nmore detail\n{head}\nmore detail\n{head}\nmore detail\n");

        assert_eq!(
            parse_warnings(&captured),
            vec![format!("{head}\nmore detail")],
            "a prefixed warning keeps its own block, so the copies collapse"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_keeps_draining_past_the_pipe_buffer() {
        use std::io::Write;

        let (result, warnings) = capture_warnings_during(|| {
            let mut stderr = std::io::stderr().lock();
            for i in 0..256 {
                let line = format!("warning: {i} {}\n", "x".repeat(1015));
                stderr.write_all(line.as_bytes()).expect("stderr write");
            }
            7
        });

        assert_eq!(result, 7);
        assert_eq!(warnings.len(), 256);
    }
}
