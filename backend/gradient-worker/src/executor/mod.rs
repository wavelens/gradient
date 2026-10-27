/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod abort;
pub mod build;
mod build_metrics;
pub mod compress;
mod dependencies;
mod download;
pub mod eval;
mod eval_cache_sharing;
pub mod eval_cache_sync;
pub mod eval_split;
pub(crate) mod failure;
pub mod fetch;
pub(crate) mod input_fetch;
pub mod log_limit;
mod progress_report;
mod source;
mod substitute;
pub mod timeline;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    BuildJob, BuildOutput, BuildProgressPhase, BuildSpec, BuildSpecKind, BuildStage, FlakeJob,
    FlakeStep,
};
use gradient_wire::types::ExecJob;
use tracing::instrument;

use gradient_wire::messages::JobPhase;

use crate::count::clamp_to_u32;
use crate::exec::host::ExecHost;
use crate::executor::timeline::PhaseGuard;
use crate::nix::gcroots::{GcRootHandle, GcRootKeeper};
use crate::nix::store::LocalNixStore;
use crate::proto::progress::{Progress, Tally};
use crate::proto::{credentials::CredentialStore, job::JobUpdater};
use gradient_wire::messages::CachedPath;
use gradient_wire::traits::WorkerStore;
use gradient_worker_client::nar;

pub use abort::AbortSignal;
pub use build_metrics::BuildHost;
pub(crate) use build_metrics::peak_ram_of_running_builds;
pub use eval::WorkerEvaluator;

async fn query_fetched_paths(
    updater: &JobUpdater,
    all_paths: Vec<String>,
    sizes: Vec<Option<u64>>,
) -> Result<Vec<CachedPath>> {
    if all_paths.is_empty() {
        return Ok(vec![]);
    }
    updater.query_push(all_paths, sizes).await
}

#[instrument(level = "debug", skip_all, fields(paths = paths.len()))]
pub(crate) async fn push_paths(
    paths: &[(String, Option<u64>)],
    updater: &JobUpdater,
    store: &LocalNixStore,
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut guard = updater.phase(JobPhase::DrvClosurePush);
    guard.record(clamp_to_u32(paths.len()), 0);
    let (paths, sizes): (Vec<String>, Vec<Option<u64>>) = paths.iter().cloned().unzip();
    let cache_entries = query_fetched_paths(updater, paths, sizes).await?;
    upload_all(updater, pair_with_store(cache_entries, store), None).await?;
    Ok(())
}

pub(crate) struct NarUpload<'a> {
    pub cached: CachedPath,
    pub source: nar::NarSource<'a>,
    pub tally: Tally,
}

fn pair_with_store<'a>(entries: Vec<CachedPath>, store: &'a LocalNixStore) -> Vec<NarUpload<'a>> {
    entries
        .into_iter()
        .map(|cached| NarUpload {
            cached,
            source: nar::NarSource::Path { meta: Some(store) },
            tally: Tally::default(),
        })
        .collect()
}

async fn upload_one_nar(
    updater: &JobUpdater,
    upload: NarUpload<'_>,
    spans: &PushSpans<'_>,
) -> Result<nar::UploadedNar> {
    let cp = &upload.cached;
    if cp.cached {
        tracing::debug!(store_path = %cp.path, "skipping NAR upload - already cached");
        return Ok(nar::UploadedNar::default());
    }
    let mut counted = Progress::counting(upload.tally);
    let uploaded = nar::upload_nar(
        &updater.uploads,
        &updater.job_id,
        &cp.path,
        upload.source,
        &mut |event| match event {
            nar::UploadEvent::Granted => spans.granted(),
            nar::UploadEvent::Read(read) => counted.at(read),
        },
    )
    .await?;
    counted.transfer_done();
    Ok(uploaded)
}

/// The batch is getting one span pair because the uploads overlap. Per-path spans would chart as
/// nested and double count.
struct PushSpans<'a> {
    updater: &'a JobUpdater,
    waiting: Mutex<Option<PhaseGuard>>,
    pushing: Mutex<Option<(PhaseGuard, Instant)>>,
}

impl<'a> PushSpans<'a> {
    fn start(updater: &'a JobUpdater) -> Self {
        Self {
            updater,
            waiting: Mutex::new(Some(updater.phase(JobPhase::UploadWait))),
            pushing: Mutex::new(None),
        }
    }

    fn granted(&self) {
        let Some(wait) = self.waiting.lock().take() else {
            return;
        };

        drop(wait);
        *self.pushing.lock() = Some((self.updater.phase(JobPhase::NarPush), Instant::now()));
    }

    fn finish(self, paths: usize, uploaded: nar::UploadedNar) {
        if let Some((mut push, started)) = self.pushing.into_inner() {
            gradient_worker_client::throughput::UPLOAD
                .observe_transfer(uploaded.nar_size, started.elapsed());
            push.record(clamp_to_u32(paths), uploaded.file_size);
        }
    }
}

pub(crate) async fn upload_all(
    updater: &JobUpdater,
    uploads: Vec<NarUpload<'_>>,
    abort: Option<&AbortSignal>,
) -> Result<nar::UploadedNar> {
    use futures::stream::{FuturesUnordered, StreamExt as _};

    let pending = uploads.iter().filter(|u| !u.cached.cached).count();
    if pending == 0 {
        return Ok(nar::UploadedNar::default());
    }

    let spans = PushSpans::start(updater);
    let mut uploaded = nar::UploadedNar::default();
    let mut running: FuturesUnordered<_> = uploads
        .into_iter()
        .map(|upload| upload_unless_aborted(updater, upload, abort, &spans))
        .collect();
    while let Some(result) = running.next().await {
        uploaded += result?;
    }

    drop(running);
    spans.finish(pending, uploaded);
    Ok(uploaded)
}

async fn upload_unless_aborted(
    updater: &JobUpdater,
    upload: NarUpload<'_>,
    abort: Option<&AbortSignal>,
    spans: &PushSpans<'_>,
) -> Result<nar::UploadedNar> {
    if let Some(abort) = abort {
        abort.check()?;
    }

    let result = upload_one_nar(updater, upload, spans).await;
    if result.is_err()
        && let Some(abort) = abort
    {
        abort.check()?;
    }
    result
}

async fn push_inputs(
    updater: JobUpdater,
    store: Arc<LocalNixStore>,
    input_paths: Vec<String>,
) -> Result<()> {
    let sizes = vec![None; input_paths.len()];
    let cache_entries = query_fetched_paths(&updater, input_paths, sizes).await?;
    let mut push = updater.phase(JobPhase::PushInputs);
    push.record(clamp_to_u32(cache_entries.len()), 0);
    upload_all(&updater, pair_with_store(cache_entries, &store), None).await?;
    Ok(())
}

fn named_outputs(task: &BuildSpec) -> Vec<(String, String)> {
    task.outputs
        .iter()
        .filter(|o| !o.path.is_empty())
        .map(|o| (o.name.clone(), o.path.clone()))
        .collect()
}

async fn split_already_realised<S: WorkerStore + ?Sized>(
    store: &S,
    wanted: Vec<(String, String)>,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut realised = Vec::new();
    let mut fetch = Vec::new();
    for (name, path) in wanted {
        if store.has_path(&path).await.unwrap_or(false) {
            realised.push((name, path));
        } else {
            fetch.push((name, path));
        }
    }

    (realised, fetch)
}

fn fully_realised(realised: &[(String, String)], missing: &[(String, String)]) -> bool {
    missing.is_empty() && !realised.is_empty()
}

async fn realised_outputs(realised: &[(String, String)]) -> Vec<BuildOutput> {
    let mut outputs = Vec::with_capacity(realised.len());
    for (name, store_path) in realised {
        outputs.push(realised_output(name, store_path).await);
    }
    outputs
}

async fn realised_output(name: &str, store_path: &str) -> BuildOutput {
    BuildOutput {
        name: name.to_owned(),
        store_path: store_path.to_owned(),
        hash: gradient_sources::get_hash_from_path(store_path.to_owned())
            .map(|(h, _)| h)
            .unwrap_or_default(),
        nar_size: None,
        nar_hash: None,
        products: build::load_products(store_path).await,
    }
}

#[derive(Clone)]
pub struct JobExecutor {
    pub(crate) store: Arc<LocalNixStore>,
    pub(crate) evaluator: Arc<WorkerEvaluator>,
    pub(crate) gcroots: GcRootKeeper,
    pub(crate) binpath_ssh: String,
    pub(crate) log_limits: crate::executor::log_limit::LogRateLimits,
    pub(crate) log_fetch_from_store: bool,
    pub(crate) host: BuildHost,
    pub(crate) exec: Option<Arc<ExecHost>>,
}

impl JobExecutor {
    #[allow(
        clippy::too_many_arguments,
        reason = "arg-heavy; refactor tracked in #503"
    )]
    pub fn new(
        store: LocalNixStore,
        evaluator: WorkerEvaluator,
        gcroots: GcRootKeeper,
        binpath_ssh: String,
        log_limits: crate::executor::log_limit::LogRateLimits,
        log_fetch_from_store: bool,
        host: BuildHost,
    ) -> Self {
        Self {
            store: Arc::new(store),
            evaluator: Arc::new(evaluator),
            gcroots,
            binpath_ssh,
            log_limits,
            log_fetch_from_store,
            host,
            exec: None,
        }
    }

    #[expect(dead_code, reason = "the worker builds its exec host in the next step")]
    pub fn with_exec(self, exec: Option<Arc<ExecHost>>) -> Self {
        Self { exec, ..self }
    }

    pub async fn shutdown(&self) {
        self.evaluator.shutdown().await;
    }

    #[instrument(name = "job", skip_all, fields(job_id = %updater.job_id, steps = ?job.steps))]
    pub async fn execute_flake_job(
        &self,
        job: FlakeJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
        abort: AbortSignal,
        split: Option<eval_split::SplitMember>,
    ) -> Result<()> {
        if let Some(member) = split {
            return self
                .execute_split_member(member, &job, updater, credentials, &abort)
                .await;
        }

        let fetches = job
            .steps
            .iter()
            .any(|step| matches!(step, FlakeStep::FetchFlake));
        if !fetches {
            return self.evaluate_steps(&job, None, updater, &abort).await;
        }

        let fetched = self.fetch_flake(&job, updater, credentials, &abort).await?;
        updater
            .report_fetch_result(Some(fetched.source_path.clone()))
            .await?;
        let push = push_inputs(
            updater.concurrent(),
            Arc::clone(&self.store),
            fetched.input_paths,
        );

        let evaluates = job
            .steps
            .iter()
            .any(|step| !matches!(step, FlakeStep::FetchFlake));
        if evaluates {
            let evaluate = self.evaluate_steps(&job, Some(&fetched.source_path), updater, &abort);
            tokio::try_join!(push, evaluate)?;
            return Ok(());
        }

        let sink = gradient_wire::traits::JobReporter::eval_progress_sink(&*updater);
        progress_report::resend_during(&*sink, fetched.progress, push).await
    }

    async fn execute_split_member(
        &self,
        member: eval_split::SplitMember,
        job: &FlakeJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
        abort: &AbortSignal,
    ) -> Result<()> {
        let context = eval_split::SplitContext {
            evaluator: &self.evaluator,
            store: &self.store,
            job,
            abort,
        };
        if member == eval_split::SplitMember::Companion {
            return eval_split::run_companion(context, updater).await;
        }

        let fetched = self.fetch_flake(job, updater, credentials, abort).await?;
        updater
            .report_fetch_result(Some(fetched.source_path.clone()))
            .await?;
        {
            let _g = updater.phase(JobPhase::EvalFlake);
            eval::evaluate_flake(job, updater).await?;
        }

        let push = push_inputs(
            updater.concurrent(),
            Arc::clone(&self.store),
            fetched.input_paths,
        );
        let _g = updater.phase(JobPhase::EvalDerivations);
        eval_split::run_primary(context, &fetched.source_path, push, updater).await
    }

    async fn evaluate_steps(
        &self,
        job: &FlakeJob,
        local_flake_path: Option<&str>,
        updater: &mut JobUpdater,
        abort: &AbortSignal,
    ) -> Result<()> {
        for step in &job.steps {
            match step {
                FlakeStep::FetchFlake => {}
                FlakeStep::EvaluateFlake => {
                    let _g = updater.phase(JobPhase::EvalFlake);
                    eval::evaluate_flake(job, updater).await?
                }
                FlakeStep::EvaluateDerivations => {
                    if local_flake_path.is_none()
                        && let Some(src) = eval::required_local_source(&job.source)
                    {
                        crate::proto::prefetch::ensure_path(&self.store, src, updater).await?;
                    }

                    let _g = updater.phase(JobPhase::EvalDerivations);
                    eval::evaluate_derivations(
                        &self.evaluator,
                        job,
                        local_flake_path,
                        updater,
                        &mut abort.clone(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn fetch_flake(
        &self,
        job: &FlakeJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
        abort: &AbortSignal,
    ) -> Result<fetch::FetchOutcome> {
        let _fetch = updater.phase(JobPhase::Fetch);
        if let Some(src) = eval::required_local_source(&job.source) {
            crate::proto::prefetch::ensure_path(&self.store, src, updater).await?;
        }

        fetch::fetch_repository(
            job,
            updater as &mut dyn gradient_wire::traits::JobReporter,
            credentials,
            &*self.store,
            abort.clone(),
        )
        .await
    }

    async fn collect_outputs<'a>(
        &'a self,
        build: &BuildSpec,
        updater: &mut JobUpdater,
        abort: &mut AbortSignal,
        gc_handles: &mut Vec<GcRootHandle>,
    ) -> Result<Vec<compress::OutputNar<'a>>> {
        abort.check()?;
        // The build must reach `Building` before anything that can fail. The server is only
        // accepting `Building -> Failed`. An earlier `JobFailed` would leave the build in
        // `Queued` forever.
        updater.report_building(build.build_id.clone()).await?;

        let (realised, missing) =
            split_already_realised(self.store.as_ref(), named_outputs(build)).await;
        if fully_realised(&realised, &missing) {
            return self
                .adopt_realised(build, realised, updater, gc_handles)
                .await;
        }
        match build.kind {
            BuildSpecKind::Substitute => {
                self.substitute(build, realised, &missing, updater, gc_handles)
                    .await
            }
            BuildSpecKind::Download => self.download(build, updater).await,
            BuildSpecKind::Build => self.build_locally(build, updater, abort, gc_handles).await,
        }
    }

    async fn adopt_realised<'a>(
        &'a self,
        build: &BuildSpec,
        realised: Vec<(String, String)>,
        updater: &mut JobUpdater,
        gc_handles: &mut Vec<GcRootHandle>,
    ) -> Result<Vec<compress::OutputNar<'a>>> {
        if self.log_fetch_from_store {
            build::forward_store_build_log(updater, &build.drv_path).await;
        }
        for (_, path) in &realised {
            gc_handles.push(self.gcroots.add(path).await);
        }
        let reported = realised_outputs(&realised).await;
        updater
            .report_build_output(build.build_id.clone(), reported, None, true)
            .await?;
        Ok(realised
            .into_iter()
            .map(|(_, store_path)| compress::OutputNar {
                build_id: build.build_id.clone(),
                store_path,
                source: nar::NarSource::Path {
                    meta: Some(&*self.store),
                },
            })
            .collect())
    }

    async fn substitute<'a>(
        &'a self,
        build: &BuildSpec,
        realised: Vec<(String, String)>,
        missing: &[(String, String)],
        updater: &mut JobUpdater,
        gc_handles: &mut Vec<GcRootHandle>,
    ) -> Result<Vec<compress::OutputNar<'a>>> {
        for (_, path) in &realised {
            gc_handles.push(self.gcroots.add(path).await);
        }

        let mut progress =
            updater.build_progress(build.build_id.clone(), BuildProgressPhase::Download);
        let fetched = substitute::fetch_outputs(
            &mut substitute::JobUpdaterIo(updater),
            missing,
            &mut progress,
        )
        .await
        .map_err(|e| failure::classify_substitute_failure(&build.build_id, e))?;

        let mut reported = realised_outputs(&realised).await;
        reported.extend(fetched.iter().map(|f| {
            BuildOutput {
                name: f.name.clone(),
                store_path: f.store_path.clone(),
                hash: gradient_sources::get_hash_from_path(f.store_path.clone())
                    .map(|(h, _)| h)
                    .unwrap_or_default(),
                nar_size: f.nar.as_ref().map(|n| n.nar_size as i64),
                nar_hash: f.nar.as_ref().map(|n| n.nar_hash.clone()),
                products: Vec::new(),
            }
        }));
        updater
            .report_build_output(build.build_id.clone(), reported, None, true)
            .await?;

        let local = realised
            .into_iter()
            .map(|(_, store_path)| compress::OutputNar {
                build_id: build.build_id.clone(),
                store_path,
                source: nar::NarSource::Path {
                    meta: Some(&*self.store),
                },
            });
        let streamed = fetched.into_iter().filter_map(|f| {
            f.nar.map(|raw| compress::OutputNar {
                build_id: build.build_id.clone(),
                store_path: f.store_path,
                source: nar::NarSource::Stream {
                    nar: raw.nar,
                    nar_size: raw.nar_size,
                    references: raw.references,
                    deriver: raw.deriver,
                    ca: raw.ca,
                },
            })
        });
        Ok(local.chain(streamed).collect())
    }

    async fn download<'a>(
        &'a self,
        build: &BuildSpec,
        updater: &mut JobUpdater,
    ) -> Result<Vec<compress::OutputNar<'a>>> {
        let (store_path, raw) = {
            let _phase = updater.phase(JobPhase::Download);
            let mut progress =
                updater.build_progress(build.build_id.clone(), BuildProgressPhase::Download);
            download::download_output(&mut download::JobUpdaterIo(updater), build, &mut progress)
                .await
                .map_err(|e| failure::classify_download_failure(&build.build_id, e))?
        };
        let reported = vec![BuildOutput {
            name: "out".to_owned(),
            store_path: store_path.clone(),
            hash: gradient_sources::get_hash_from_path(store_path.clone())
                .map(|(h, _)| h)
                .unwrap_or_default(),
            nar_size: Some(raw.nar_size as i64),
            nar_hash: Some(raw.nar_hash.clone()),
            products: Vec::new(),
        }];
        updater
            .report_build_output(build.build_id.clone(), reported, None, false)
            .await?;
        Ok(vec![compress::OutputNar {
            build_id: build.build_id.clone(),
            store_path,
            source: nar::NarSource::Stream {
                nar: raw.nar,
                nar_size: raw.nar_size,
                references: raw.references,
                deriver: raw.deriver,
                ca: raw.ca,
            },
        }])
    }

    async fn build_locally<'a>(
        &'a self,
        build: &BuildSpec,
        updater: &mut JobUpdater,
        abort: &mut AbortSignal,
        gc_handles: &mut Vec<GcRootHandle>,
    ) -> Result<Vec<compress::OutputNar<'a>>> {
        gc_handles.push(self.gcroots.add(&build.drv_path).await);

        updater.report_stage(BuildStage::Prefetch).await?;
        {
            let mut prefetch = updater.phase(JobPhase::Prefetch);
            let fetched = crate::proto::prefetch::prefetch_inputs(&self.store, build, updater)
                .await
                .map_err(|e| failure::classify_prefetch_error(&build.build_id, e))?;
            prefetch.record(fetched.paths, fetched.bytes);
        }

        updater.report_stage(BuildStage::Build).await?;
        let _build = updater.phase(JobPhase::Build);
        let built = build::build_derivation(
            &self.store,
            build,
            updater,
            abort,
            self.log_limits,
            self.log_fetch_from_store,
            self.host,
        )
        .await?;
        for o in &built {
            gc_handles.push(self.gcroots.add(&o.store_path).await);
        }
        Ok(built
            .into_iter()
            .map(|o| compress::OutputNar {
                build_id: build.build_id.clone(),
                store_path: o.store_path,
                source: nar::NarSource::Path {
                    meta: Some(&*self.store),
                },
            })
            .collect())
    }

    #[instrument(skip_all)]
    pub async fn execute_build_job(
        &self,
        job: BuildJob,
        updater: &mut JobUpdater,
        _credentials: &CredentialStore,
        mut abort: AbortSignal,
    ) -> Result<()> {
        let mut gc_handles: Vec<GcRootHandle> = Vec::new();
        let outputs = self
            .collect_outputs(&job.build, updater, &mut abort, &mut gc_handles)
            .await?;

        {
            let mut compress = updater.phase(JobPhase::Compress);
            compress.record(clamp_to_u32(outputs.len()), 0);
            let packed = compress::push_outputs(updater, outputs, &abort)
                .await
                .map_err(failure::BuildError::transient)?;
            compress.record(0, packed.nar_size);
        }

        drop(gc_handles);
        Ok(())
    }

    #[instrument(skip_all, fields(job_id = %updater.job_id))]
    pub async fn execute_exec_job(
        &self,
        job: ExecJob,
        updater: &mut JobUpdater,
        credentials: &CredentialStore,
        abort: AbortSignal,
    ) -> Result<()> {
        let host = self
            .exec
            .as_deref()
            .context("this worker runs no exec jobs")?;
        crate::exec::job::execute(host, &self.store, job, updater, credentials, abort).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_test_support::fakes::worker_store::FakeWorkerStore;

    fn spec(kind: BuildSpecKind, outputs: &[(&str, &str)]) -> BuildSpec {
        BuildSpec {
            build_id: "b".to_owned(),
            drv_path: "/nix/store/x.drv".to_owned(),
            kind,
            is_fixed_output: false,
            outputs: outputs
                .iter()
                .map(|(name, path)| gradient_wire::messages::DerivationOutput {
                    name: (*name).to_owned(),
                    path: (*path).to_owned(),
                })
                .collect(),
            timeout_secs: None,
            max_silent_secs: None,
            build_cores: None,
            input_realisations: Vec::new(),
        }
    }

    #[test]
    fn named_outputs_drops_the_ones_with_no_path() {
        let task = spec(
            BuildSpecKind::Substitute,
            &[("out", "/nix/store/a-out"), ("dev", "")],
        );

        assert_eq!(
            named_outputs(&task),
            vec![("out".to_owned(), "/nix/store/a-out".to_owned())]
        );
    }

    #[tokio::test]
    async fn what_the_store_already_holds_is_not_fetched() {
        let store = FakeWorkerStore::new().with_present_path("/nix/store/a-out");
        let wanted = vec![
            ("out".to_owned(), "/nix/store/a-out".to_owned()),
            ("dev".to_owned(), "/nix/store/b-dev".to_owned()),
        ];

        let (realised, missing) = split_already_realised(&store, wanted).await;

        assert_eq!(
            realised,
            vec![("out".to_owned(), "/nix/store/a-out".to_owned())]
        );
        assert_eq!(
            missing,
            vec![("dev".to_owned(), "/nix/store/b-dev".to_owned())]
        );
    }

    struct NoDaemon;

    #[async_trait::async_trait]
    impl WorkerStore for NoDaemon {
        async fn has_path(&self, _store_path: &str) -> Result<bool> {
            Err(anyhow::anyhow!("acquire daemon connection: no such file"))
        }

        async fn add_nar(&self, _name: &str, _nar: Vec<u8>) -> Result<String> {
            Err(anyhow::anyhow!("acquire daemon connection: no such file"))
        }
    }

    #[tokio::test]
    async fn a_worker_without_a_daemon_fetches_everything() {
        let wanted = vec![("out".to_owned(), "/nix/store/a-out".to_owned())];

        let (realised, missing) = split_already_realised(&NoDaemon, wanted.clone()).await;

        assert!(realised.is_empty());
        assert_eq!(missing, wanted);
    }

    #[test]
    fn only_a_build_with_every_output_on_disk_is_skipped() {
        let out = || ("out".to_owned(), "/nix/store/a-out".to_owned());
        let dev = || ("dev".to_owned(), "/nix/store/b-dev".to_owned());

        assert!(fully_realised(&[out(), dev()], &[]));
        assert!(!fully_realised(&[out()], &[dev()]));
        assert!(!fully_realised(&[], &[]));
    }

    #[tokio::test]
    async fn a_realised_output_reports_its_hydra_products() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().to_str().unwrap();
        let tarball = dir.path().join("foo.tar.gz");
        std::fs::write(&tarball, b"tar").unwrap();
        std::fs::create_dir(dir.path().join("nix-support")).unwrap();
        std::fs::write(
            dir.path().join("nix-support/hydra-build-products"),
            format!("file binary-dist {}\n", tarball.display()),
        )
        .unwrap();

        let out = realised_output("out", store_path).await;

        assert_eq!(out.products.len(), 1);
        assert_eq!(out.products[0].name, "foo.tar.gz");
        assert_eq!(out.products[0].size, Some(3));
    }
}
