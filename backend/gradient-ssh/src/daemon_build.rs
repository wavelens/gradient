/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_wait::{BuildOutcome, DrvResult};
use crate::session::Session;
use crate::store::err;
use crate::store_paths::{original, stored};
use gradient_db::cache_paths::served_hashes;
use gradient_types::StorePathMapper;
use harmonia_protocol::daemon::wire::types2::{
    BuildResult, BuildResultFailure, BuildResultInner, BuildResultSuccess, KeyedBuildResult,
    SuccessStatus,
};
use harmonia_protocol::daemon::{DaemonResult, FutureResultExt as _, ResultLog};
use harmonia_protocol::log::LogMessage;
use harmonia_store_derivation::derivation::BasicDerivation;
use harmonia_store_derivation::derived_path::{DerivedPath, SingleDerivedPath};
use harmonia_store_derivation::realisation::UnkeyedRealisation;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc;

fn streamed<T, W, F>(
    session: Arc<Session>,
    work: W,
) -> impl ResultLog<Output = DaemonResult<T>> + Send + 'static
where
    T: Send + 'static,
    W: FnOnce(Arc<Session>, Box<dyn Fn(LogMessage) + Send + Sync>) -> F + Send + 'static,
    F: Future<Output = DaemonResult<T>> + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<LogMessage>();
    let shutdown = session.handles.db.shutdown.clone();
    let log = Box::new(move |message: LogMessage| {
        let _ = tx.send(message);
    });
    let task = shutdown.spawn(work(session, log));

    let logs = async_stream::stream! {
        while let Some(msg) = rx.recv().await {
            yield msg;
        }
    };

    async move { task.await.map_err(err)? }.with_logs(logs)
}

pub fn build_paths(
    session: Arc<Session>,
    paths: Vec<DerivedPath>,
) -> impl ResultLog<Output = DaemonResult<Vec<KeyedBuildResult>>> + Send + 'static {
    streamed(session, move |session, log| async move {
        let mapper = session.store_path_mapper();
        let derivations = requested_derivations(&session, &mapper, &paths).await?;
        let outcome = if derivations.is_empty() {
            None
        } else {
            Some(
                crate::build_request::run(&session, &derivations, log)
                    .await
                    .map_err(err)?,
            )
        };

        Ok(keyed_results(&mapper, &paths, outcome))
    })
}

pub fn build_derivation(
    session: Arc<Session>,
    drv: BasicDerivation,
) -> impl ResultLog<Output = DaemonResult<BuildResult>> + Send + 'static {
    streamed(session, move |session, log| async move {
        let drv_path = crate::basic_derivation::import(&session, &drv)
            .await
            .map_err(err)?;
        let outcome = crate::build_request::run(&session, std::slice::from_ref(&drv_path), log)
            .await
            .map_err(err)?;
        let result = outcome.results.iter().find(|(path, _)| *path == drv_path);
        Ok(build_result(result_inner(
            &session.store_path_mapper(),
            result.map(|(_, r)| r),
        )))
    })
}

async fn requested_derivations(
    session: &Session,
    mapper: &StorePathMapper,
    paths: &[DerivedPath],
) -> DaemonResult<Vec<String>> {
    let mut derivations = Vec::new();
    let mut opaque = Vec::new();
    for path in paths {
        match path {
            DerivedPath::Built { drv_path, .. } => match drv_path.as_ref() {
                SingleDerivedPath::Opaque(drv) => {
                    derivations.push(stored(mapper, drv).map_err(err)?.full())
                }
                SingleDerivedPath::Built { .. } => {
                    return Err(err("dynamic derivations are not supported over SSH"));
                }
            },
            DerivedPath::Opaque(p) => opaque.push(p),
        }
    }

    let hashes: Vec<String> = opaque
        .iter()
        .map(|p| stored(mapper, p).map(|s| s.hash().to_owned()))
        .collect::<anyhow::Result<_>>()
        .map_err(err)?;
    let served = served_hashes(&session.handles.db.web_db, &session.caches, &hashes)
        .await
        .map_err(err)?;
    if let Some((missing, _)) = opaque
        .iter()
        .zip(&hashes)
        .find(|(_, hash)| !served.contains(*hash))
    {
        return Err(err(format!("{missing} is not in the project caches")));
    }

    Ok(derivations)
}

fn keyed_results(
    mapper: &StorePathMapper,
    paths: &[DerivedPath],
    outcome: Option<BuildOutcome>,
) -> Vec<KeyedBuildResult> {
    let by_drv: HashMap<String, DrvResult> = outcome
        .map(|o| o.results.into_iter().collect())
        .unwrap_or_default();

    paths
        .iter()
        .map(|path| {
            let inner = match path {
                DerivedPath::Built { drv_path, .. } => match drv_path.as_ref() {
                    SingleDerivedPath::Opaque(drv) => result_inner(
                        mapper,
                        stored(mapper, drv)
                            .ok()
                            .and_then(|drv| by_drv.get(&drv.full())),
                    ),
                    SingleDerivedPath::Built { .. } => already_valid(),
                },
                DerivedPath::Opaque(_) => already_valid(),
            };

            KeyedBuildResult {
                path: path.clone(),
                result: build_result(inner),
            }
        })
        .collect()
}

fn result_inner(mapper: &StorePathMapper, result: Option<&DrvResult>) -> BuildResultInner {
    match result {
        Some(Ok(outputs)) => built(mapper, outputs),
        Some(Err(failure)) => BuildResultInner::Failure(BuildResultFailure {
            status: failure.status,
            error_msg: failure.message.clone().into_bytes(),
            is_non_deterministic: false,
        }),
        None => already_valid(),
    }
}

fn build_result(inner: BuildResultInner) -> BuildResult {
    BuildResult {
        inner,
        times_built: 0,
        start_time: 0,
        stop_time: 0,
        cpu_user: None,
        cpu_system: None,
        memory_peak: None,
        io_read_bytes: None,
        io_write_bytes: None,
        oom_kills: None,
    }
}

fn built(mapper: &StorePathMapper, outputs: &BTreeMap<String, String>) -> BuildResultInner {
    let built_outputs = outputs
        .iter()
        .filter_map(|(name, path)| {
            let out_path = original(mapper, path).ok()?;
            Some((
                name.parse().ok()?,
                UnkeyedRealisation {
                    out_path,
                    signatures: Default::default(),
                },
            ))
        })
        .collect();

    BuildResultInner::Success(BuildResultSuccess {
        status: SuccessStatus::Built,
        built_outputs,
    })
}

fn already_valid() -> BuildResultInner {
    BuildResultInner::Success(BuildResultSuccess {
        status: SuccessStatus::AlreadyValid,
        built_outputs: BTreeMap::new(),
    })
}
