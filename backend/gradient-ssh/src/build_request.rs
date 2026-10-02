/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_wait::{BuildOutcome, wait};
use crate::session::Session;
use futures::StreamExt as _;
use gradient_db::build_request_task::ensure_build_request_task;
use gradient_db::permissions::Permission;
use gradient_derivation::{Derivation, discovered_derivation, parse_drv};
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_graph::{RecordBatch, Transition};
use gradient_types::*;
use gradient_util::store_path::strip_nix_store_prefix;
use gradient_wire::types::DiscoveredDerivation;
use sea_orm::{ActiveModelTrait, IntoActiveModel};
use std::collections::HashSet;
use std::future::Future;
use tokio::io::AsyncReadExt as _;

const CONCURRENT_READS: usize = 32;

pub trait DrvSource: Sync {
    fn read(
        &self,
        drv_path: &str,
    ) -> impl Future<Output = anyhow::Result<Option<Derivation>>> + Send;
}

struct CacheDrvSource<'a> {
    session: &'a Session,
}

impl DrvSource for CacheDrvSource<'_> {
    async fn read(&self, drv_path: &str) -> anyhow::Result<Option<Derivation>> {
        let base = strip_nix_store_prefix(drv_path);
        let hash = base.split('-').next().unwrap_or_default();
        let state = &self.session.state;
        if gradient_db::cache_paths::served_path(&state.web_db, &self.session.caches, hash)
            .await?
            .is_none()
        {
            return Ok(None);
        }

        let Some(reader) = crate::nar::open_raw(state, hash).await? else {
            return Ok(None);
        };

        let mut events = std::pin::pin!(harmonia_file_nar::parse_nar(reader));
        while let Some(event) = events.next().await {
            if let harmonia_file_nar::NarEvent::File { mut reader, .. } = event? {
                let mut contents = Vec::new();
                reader.read_to_end(&mut contents).await?;
                return parse_drv(&contents).map(Some);
            }
        }

        anyhow::bail!("{drv_path} is not a single-file NAR")
    }
}

pub fn drv_name(drv_path: &str) -> &str {
    let base = drv_path.strip_prefix("/nix/store/").unwrap_or(drv_path);
    let name = base.split_once('-').map_or(base, |(_, name)| name);
    name.strip_suffix(".drv").unwrap_or(name)
}

pub async fn closure(
    source: &impl DrvSource,
    roots: &[String],
) -> anyhow::Result<Vec<(String, Derivation)>> {
    let mut seen: HashSet<String> = roots.iter().cloned().collect();
    let mut frontier = roots.to_vec();
    let mut found = Vec::new();
    while !frontier.is_empty() {
        let read: Vec<_> = futures::stream::iter(std::mem::take(&mut frontier))
            .map(|path| async move { (source.read(&path).await, path) })
            .buffer_unordered(CONCURRENT_READS)
            .collect()
            .await;

        for (drv, path) in read {
            let drv = drv?.ok_or_else(|| {
                anyhow::anyhow!("{path} is not in the project caches, copy it first")
            })?;
            anyhow::ensure!(
                drv.outputs.iter().all(|o| !o.path.is_empty()),
                "{path} is content-addressed, not supported over SSH"
            );

            for (input, _) in &drv.input_derivations {
                if seen.insert(input.clone()) {
                    frontier.push(input.clone());
                }
            }

            found.push((path, drv));
        }
    }

    Ok(found)
}

pub async fn start(
    session: &Session,
    drv_paths: &[String],
) -> anyhow::Result<(EvaluationId, Vec<(String, Derivation)>)> {
    anyhow::ensure!(
        session.may(Permission::TriggerEvaluation),
        "building in {} needs TriggerEvaluation",
        session.project.name
    );

    let state = &session.state;
    let closure = closure(&CacheDrvSource { session }, drv_paths).await?;
    let task = ensure_build_request_task(
        &state.web_db,
        session.project.id,
        session.user.id,
        state.config.eval.default_keep_evaluations(),
    )
    .await?;

    let names: Vec<&str> = drv_paths.iter().map(|p| drv_name(p)).collect();
    let commit = MCommit {
        id: CommitId::now_v7(),
        message: format!("SSH build of {}", names.join(" ")),
        hash: vec![0; 20],
        author: Some(session.user.id),
        author_name: session.user.name.clone(),
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    let created = now();
    let evaluation = MEvaluation {
        id: EvaluationId::now_v7(),
        task: Some(task.id),
        repository: "ssh".into(),
        commit: commit.id,
        wildcard: names.join(" "),
        status: EvaluationStatus::EvaluatingDerivation,
        kind: EvaluationKind::Ssh,
        started_by: Some(session.user.id),
        concurrent: true,
        created_at: created,
        updated_at: created,
        eval_drv_started_at: Some(created),
        ..Default::default()
    }
    .into_active_model()
    .insert(&state.web_db)
    .await?;

    let requested: HashSet<&str> = drv_paths.iter().map(String::as_str).collect();
    let derivations: Vec<DiscoveredDerivation> = closure
        .iter()
        .map(|(path, drv)| {
            let attr = requested
                .contains(path.as_str())
                .then(|| drv_name(path).to_string());
            let mut discovered = discovered_derivation(attr, strip_nix_store_prefix(path), drv);
            for dependency in &mut discovered.dependencies {
                *dependency = strip_nix_store_prefix(dependency);
            }

            discovered
        })
        .collect();

    let truly_substituted = gradient_scheduler::eval::assess_cached(state, &derivations).await;
    state
        .graph
        .record(RecordBatch {
            evaluation: evaluation.id,
            task: Some(task.id),
            derivations,
            warnings: vec![],
            errors: vec![],
            truly_substituted,
        })
        .await?;
    state
        .graph
        .transition(Transition::EvalStreamCompleted {
            evaluation: evaluation.id,
        })
        .await?;

    let requested_drvs = closure
        .into_iter()
        .filter(|(path, _)| requested.contains(path.as_str()))
        .collect();
    Ok((evaluation.id, requested_drvs))
}

pub async fn run(
    session: &Session,
    drv_paths: &[String],
    log: impl Fn(String) + Send + Sync,
) -> anyhow::Result<BuildOutcome> {
    let (evaluation, requested) = start(session, drv_paths).await?;
    log(format!("Gradient evaluation {evaluation}"));
    wait(session, evaluation, &requested, log).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_derivation::DerivationOutput;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Fake {
        drvs: HashMap<String, Derivation>,
        reads: Mutex<Vec<String>>,
    }

    impl DrvSource for Fake {
        async fn read(&self, drv_path: &str) -> anyhow::Result<Option<Derivation>> {
            self.reads.lock().expect("reads").push(drv_path.to_string());
            Ok(self.drvs.get(drv_path).cloned())
        }
    }

    fn path(name: &str) -> String {
        format!("/nix/store/{}-{name}.drv", "0".repeat(32))
    }

    fn drv(inputs: &[&str]) -> Derivation {
        Derivation {
            outputs: vec![DerivationOutput {
                name: "out".into(),
                path: format!("/nix/store/{}-out", "1".repeat(32)),
                hash_algo: String::new(),
                hash: String::new(),
            }],
            input_derivations: inputs
                .iter()
                .map(|i| (path(i), vec!["out".into()]))
                .collect(),
            input_sources: vec![],
            system: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            environment: HashMap::new(),
        }
    }

    fn fake(drvs: &[(&str, Derivation)]) -> Fake {
        Fake {
            drvs: drvs.iter().map(|(n, d)| (path(n), d.clone())).collect(),
            reads: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn closure_visits_each_drv_once() {
        let source = fake(&[("a", drv(&["b", "c"])), ("b", drv(&["c"])), ("c", drv(&[]))]);

        let closure = closure(&source, &[path("a")]).await.expect("closure");

        let mut found: Vec<String> = closure.into_iter().map(|(p, _)| p).collect();
        found.sort();
        assert_eq!(found, [path("a"), path("b"), path("c")]);
        assert_eq!(source.reads.lock().expect("reads").len(), 3);
    }

    #[tokio::test]
    async fn a_drv_missing_from_the_cache_names_the_path() {
        let source = fake(&[("a", drv(&["b"]))]);

        let e = closure(&source, &[path("a")]).await.expect_err("missing");
        assert!(e.to_string().contains(&path("b")), "{e}");
        assert!(e.to_string().contains("copy"), "{e}");
    }

    #[tokio::test]
    async fn a_content_addressed_derivation_is_refused() {
        let mut floating = drv(&[]);
        floating.outputs[0].path = String::new();
        let source = fake(&[("a", floating)]);

        let e = closure(&source, &[path("a")]).await.expect_err("ca");
        assert!(e.to_string().contains("content-addressed"), "{e}");
    }

    #[tokio::test]
    async fn building_without_trigger_evaluation_is_refused() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let session = Session {
            state: gradient_test_support::state::test_state(db.clone()),
            user: gradient_test_support::fixtures::user(),
            project: gradient_test_support::fixtures::project(),
            permissions: 0,
            caches: vec![],
        };

        let e = start(&session, &[path("a")]).await.expect_err("refused");
        assert!(e.to_string().contains("TriggerEvaluation"), "{e}");
        assert!(db.into_transaction_log().is_empty());
    }

    #[test]
    fn drv_names_drop_the_hash_and_suffix() {
        assert_eq!(drv_name(&path("hello-2.12")), "hello-2.12");
    }
}
