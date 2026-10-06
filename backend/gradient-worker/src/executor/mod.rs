/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod abort;
pub mod build;
mod build_metrics;
pub mod compress;
mod download;
pub mod eval;
pub(crate) mod failure;
pub mod fetch;
pub mod log_limit;
mod progress_report;
mod source;
mod substitute;
pub mod timeline;

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    BuildJob, BuildOutput, BuildProgressPhase, BuildSpec, BuildSpecKind, BuildStage, FlakeJob,
    FlakeStep,
};
use tracing::instrument;

use gradient_wire::messages::JobPhase;

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
    guard.record(paths.len() as u32, 0);
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
            push.record(paths as u32, uploaded.file_size);
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
        }
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
    ) -> Result<()> {
        let mut local_flake_path: Option<String> = None;

        for step in &job.steps {
            match step {
                FlakeStep::FetchFlake => {
                    let _fetch = updater.phase(JobPhase::Fetch);
                    if let Some(src) = eval::required_local_source(&job.source) {
                        crate::proto::prefetch::ensure_path(&self.store, src, updater).await?;
                    }

                    let outcome = fetch::fetch_repository(
                        &job,
                        updater as &mut dyn gradient_wire::traits::JobReporter,
                        credentials,
                        &*self.store,
                        &**self.evaluator.resolver(),
                        &self.binpath_ssh,
                        abort.clone(),
                    )
                    .await?;

                    let sink = gradient_wire::traits::JobReporter::eval_progress_sink(&*updater);
                    progress_report::resend_during(
                        &*sink,
                        outcome.progress,
                        self.push_inputs(updater, &outcome.input_paths, &outcome.source_path),
                    )
                    .await?;
                    local_flake_path = Some(outcome.source_path);
                }
                FlakeStep::EvaluateFlake => {
                    let _g = updater.phase(JobPhase::EvalFlake);
                    eval::evaluate_flake(&job, updater).await?
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
                        &job,
                        local_flake_path.as_deref(),
                        updater,
                        &mut abort.clone(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn push_inputs(
        &self,
        updater: &JobUpdater,
        input_paths: &[String],
        source_path: &str,
    ) -> Result<()> {
        let sizes = vec![None; input_paths.len()];
        let cache_entries = query_fetched_paths(updater, input_paths.to_vec(), sizes).await?;
        {
            let mut push = updater.phase(JobPhase::PushInputs);
            push.record(cache_entries.len() as u32, 0);
            upload_all(updater, pair_with_store(cache_entries, &self.store), None).await?;
        }

        updater
            .report_fetch_result(Some(source_path.to_owned()))
            .await
    }

    async fn adopt_realised<'a>(
        &'a self,
        build_task: &BuildSpec,
        task_index: u32,
        realised: Vec<(String, String)>,
        updater: &mut JobUpdater,
        gc_handles: &mut Vec<GcRootHandle>,
        outputs: &mut Vec<compress::OutputNar<'a>>,
    ) -> Result<()> {
        if self.log_fetch_from_store {
            build::forward_store_build_log(updater, task_index, &build_task.drv_path).await;
        }
        for (_, path) in &realised {
            gc_handles.push(self.gcroots.add(path).await);
        }
        let reported = realised_outputs(&realised).await;
        updater
            .report_build_output(build_task.build_id.clone(), reported, None, true)
            .await?;
        outputs.extend(
            realised
                .into_iter()
                .map(|(_, store_path)| compress::OutputNar {
                    build_id: build_task.build_id.clone(),
                    store_path,
                    source: nar::NarSource::Path {
                        meta: Some(&*self.store),
                    },
                }),
        );
        Ok(())
    }

    #[instrument(skip_all)]
    pub async fn execute_build_job(
        &self,
        job: BuildJob,
        updater: &mut JobUpdater,
        _credentials: &CredentialStore,
        mut abort: AbortSignal,
    ) -> Result<()> {
        let mut outputs: Vec<compress::OutputNar<'_>> = Vec::new();
        let mut gc_handles: Vec<GcRootHandle> = Vec::new();
        for (index, build_task) in job.builds.iter().enumerate() {
            abort.check()?;
            // The build must reach `Building` before anything that can fail. The server is only
            // accepting `Building -> Failed`. An earlier `JobFailed` would leave the build in
            // `Queued` forever.
            updater.report_building(build_task.build_id.clone()).await?;

            let (realised, missing) =
                split_already_realised(self.store.as_ref(), named_outputs(build_task)).await;
            if fully_realised(&realised, &missing) {
                self.adopt_realised(
                    build_task,
                    index as u32,
                    realised,
                    updater,
                    &mut gc_handles,
                    &mut outputs,
                )
                .await?;
                continue;
            }

            if build_task.kind == BuildSpecKind::Substitute {
                for (_, path) in &realised {
                    gc_handles.push(self.gcroots.add(path).await);
                }

                let mut progress = updater
                    .build_progress(build_task.build_id.clone(), BuildProgressPhase::Download);
                let fetched = substitute::fetch_outputs(
                    &mut substitute::JobUpdaterIo(updater),
                    &missing,
                    &mut progress,
                )
                .await
                .map_err(|e| failure::classify_substitute_failure(&build_task.build_id, e))?;

                let mut reported = realised_outputs(&realised).await;
                reported.extend(fetched.iter().map(|f| {
                    BuildOutput {
                        name: f.name.clone(),
                        store_path: f.store_path.clone(),
                        hash: gradient_sources::get_hash_from_path(f.store_path.clone())
                            .map(|(h, _)| h)
                            .unwrap_or_default(),
                        nar_size: f.nar.as_ref().map(|n| n.nar.len() as i64),
                        nar_hash: f.nar.as_ref().map(|n| nar::sha256_nix32(&n.nar)),
                        products: Vec::new(),
                    }
                }));
                updater
                    .report_build_output(build_task.build_id.clone(), reported, None, true)
                    .await?;

                outputs.extend(
                    realised
                        .into_iter()
                        .map(|(_, store_path)| compress::OutputNar {
                            build_id: build_task.build_id.clone(),
                            store_path,
                            source: nar::NarSource::Path {
                                meta: Some(&*self.store),
                            },
                        }),
                );
                outputs.extend(fetched.into_iter().filter_map(|f| {
                    f.nar.map(|raw| compress::OutputNar {
                        build_id: build_task.build_id.clone(),
                        store_path: f.store_path,
                        source: nar::NarSource::Raw {
                            nar: raw.nar,
                            references: raw.references,
                            deriver: raw.deriver,
                            ca: raw.ca,
                        },
                    })
                }));
                continue;
            }

            if build_task.kind == BuildSpecKind::Download {
                let (store_path, raw) = {
                    let _phase = updater.phase(JobPhase::Download);
                    let mut progress = updater
                        .build_progress(build_task.build_id.clone(), BuildProgressPhase::Download);
                    download::download_output(
                        &mut download::JobUpdaterIo(updater),
                        build_task,
                        &mut progress,
                    )
                    .await
                    .map_err(|e| failure::classify_download_failure(&build_task.build_id, e))?
                };
                let reported = vec![BuildOutput {
                    name: "out".to_owned(),
                    store_path: store_path.clone(),
                    hash: gradient_sources::get_hash_from_path(store_path.clone())
                        .map(|(h, _)| h)
                        .unwrap_or_default(),
                    nar_size: Some(raw.nar.len() as i64),
                    nar_hash: Some(nar::sha256_nix32(&raw.nar)),
                    products: Vec::new(),
                }];
                updater
                    .report_build_output(build_task.build_id.clone(), reported, None, false)
                    .await?;
                outputs.push(compress::OutputNar {
                    build_id: build_task.build_id.clone(),
                    store_path,
                    source: nar::NarSource::Raw {
                        nar: raw.nar,
                        references: raw.references,
                        deriver: raw.deriver,
                        ca: raw.ca,
                    },
                });
                continue;
            }

            gc_handles.push(self.gcroots.add(&build_task.drv_path).await);

            updater.report_stage(BuildStage::Prefetch).await?;
            {
                let mut prefetch = updater.phase(JobPhase::Prefetch);
                let fetched =
                    crate::proto::prefetch::prefetch_inputs(&self.store, build_task, updater)
                        .await
                        .map_err(|e| failure::classify_prefetch_error(&build_task.build_id, e))?;
                prefetch.record(fetched.paths, fetched.bytes);
            }

            updater.report_stage(BuildStage::Build).await?;
            let _build = updater.phase(JobPhase::Build);
            let built = build::build_derivation(
                &self.store,
                build_task,
                index as u32,
                updater,
                &mut abort,
                self.log_limits,
                self.log_fetch_from_store,
                self.host,
            )
            .await?;
            for o in &built {
                gc_handles.push(self.gcroots.add(&o.store_path).await);
            }
            outputs.extend(built.into_iter().map(|o| compress::OutputNar {
                build_id: build_task.build_id.clone(),
                store_path: o.store_path,
                source: nar::NarSource::Path {
                    meta: Some(&*self.store),
                },
            }));
        }

        {
            let mut compress = updater.phase(JobPhase::Compress);
            compress.record(outputs.len() as u32, 0);
            let packed = compress::push_outputs(updater, outputs, &abort)
                .await
                .map_err(failure::BuildError::transient)?;
            compress.record(0, packed.nar_size);
        }

        drop(gc_handles);
        Ok(())
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
