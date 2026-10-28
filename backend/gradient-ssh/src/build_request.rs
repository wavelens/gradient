/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_wait::{BuildOutcome, wait};
use crate::session::Session;
use crate::store_paths::stored_derivation;
use futures::StreamExt as _;
use gradient_db::Hold;
use gradient_db::build_request_task::ensure_build_request_task;
use gradient_db::lookup::by_id;
use gradient_derivation::{Derivation, discovered_derivation, parse_drv};
use gradient_entity::evaluation::{EvaluationKind, EvaluationStatus};
use gradient_graph::{RecordBatch, Transition};
use gradient_types::permissions::Permission;
use gradient_types::*;
use gradient_wire::types::DiscoveredDerivation;
use harmonia_protocol::log::LogMessage;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Select,
    TransactionTrait,
};
use std::collections::HashSet;
use std::future::Future;
use tokio::io::AsyncReadExt as _;

const CONCURRENT_READS: usize = 32;
const RECORD_BATCH_SIZE: usize = 50;

static EVALUATION_LOOKUP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const JOINABLE: [EvaluationStatus; 3] = [
    EvaluationStatus::EvaluatingDerivation,
    EvaluationStatus::Building,
    EvaluationStatus::Waiting,
];

struct Target {
    task: TaskId,
    evaluation: EvaluationId,
    hold: Hold,
}

enum Added {
    Yes,
    EvaluationEnded,
}

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
        let path = StorePath::parse_stored(drv_path)?;
        let hash = path.hash();
        let handles = &self.session.handles;
        if gradient_db::cache_paths::served_path(&handles.db.web_db, &self.session.caches, hash)
            .await?
            .is_none()
        {
            return Ok(None);
        }

        let Some(reader) = crate::nar::open_raw(&handles.db.storage.nar_storage, hash).await?
        else {
            return Ok(None);
        };

        let mut events = std::pin::pin!(harmonia_file_nar::parse_nar(reader));
        while let Some(event) = events.next().await {
            if let harmonia_file_nar::NarEvent::File { mut reader, .. } = event? {
                let mut contents = Vec::new();
                reader.read_to_end(&mut contents).await?;
                let drv = parse_drv(&contents)?;
                return stored_derivation(&self.session.mapper, drv).map(Some);
            }
        }

        anyhow::bail!("{drv_path} is not a single-file NAR")
    }
}

pub fn drv_name(drv_path: &str) -> String {
    let name = StorePath::parse_stored(drv_path)
        .map_or_else(|_| drv_path.to_owned(), |path| path.name().to_owned());
    match name.strip_suffix(".drv") {
        Some(name) => name.to_owned(),
        None => name,
    }
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

            for input in &drv.input_derivations {
                if seen.insert(input.path.clone()) {
                    frontier.push(input.path.clone());
                }
            }

            found.push((path, drv));
        }
    }

    Ok(found)
}

#[derive(Debug)]
pub struct Started {
    pub evaluation: EvaluationId,
    pub closure: Vec<String>,
    pub requested: Vec<(String, Derivation)>,
}

pub async fn start(session: &Session, drv_paths: &[String]) -> anyhow::Result<Started> {
    anyhow::ensure!(
        session.may(Permission::TriggerEvaluation),
        "building in {} needs TriggerEvaluation",
        session.project.name
    );

    let closure = closure(&CacheDrvSource { session }, drv_paths).await?;
    let evaluation = add_to_user_evaluation(session, &closure, drv_paths).await?;

    let requested: HashSet<&str> = drv_paths.iter().map(String::as_str).collect();
    let paths = closure.iter().map(|(path, _)| path.clone()).collect();
    let requested_drvs = closure
        .into_iter()
        .filter(|(path, _)| requested.contains(path.as_str()))
        .collect();
    Ok(Started {
        evaluation,
        closure: paths,
        requested: requested_drvs,
    })
}

async fn add_to_user_evaluation(
    session: &Session,
    closure: &[(String, Derivation)],
    drv_paths: &[String],
) -> anyhow::Result<EvaluationId> {
    let target = evaluation_for(session, drv_paths).await?;
    let evaluation = target.evaluation;
    if let Added::Yes = add(session, target, closure, drv_paths).await? {
        return Ok(evaluation);
    }

    let target = evaluation_for(session, drv_paths).await?;
    let evaluation = target.evaluation;
    match add(session, target, closure, drv_paths).await? {
        Added::Yes => Ok(evaluation),
        Added::EvaluationEnded => {
            anyhow::bail!("evaluation {evaluation} ended before the builds were added")
        }
    }
}

async fn evaluation_for(session: &Session, drv_paths: &[String]) -> anyhow::Result<Target> {
    let _lookup = EVALUATION_LOOKUP.lock().await;
    let handles = &session.handles;
    let task = ensure_build_request_task(
        &handles.db.web_db,
        session.project.id,
        session.user.id,
        handles.db.config.eval.default_keep_evaluations(),
    )
    .await?;

    let running = running_evaluation(task.id, session.user.id)
        .one(&handles.db.web_db)
        .await?;
    let evaluation = match running {
        Some(running) => running.id,
        None => create_evaluation(session, task.id, drv_paths).await?,
    };

    Ok(Target {
        task: task.id,
        evaluation,
        hold: handles.db.memory.held_evaluations.hold(evaluation).await,
    })
}

fn running_evaluation(task: TaskId, user: UserId) -> Select<EEvaluation> {
    EEvaluation::find()
        .filter(CEvaluation::Task.eq(task))
        .filter(CEvaluation::Kind.eq(EvaluationKind::Ssh))
        .filter(CEvaluation::StartedBy.eq(user))
        .filter(CEvaluation::Status.is_in(JOINABLE))
        .order_by_desc(CEvaluation::CreatedAt)
}

async fn create_evaluation(
    session: &Session,
    task: TaskId,
    drv_paths: &[String],
) -> anyhow::Result<EvaluationId> {
    let handles = &session.handles;

    let names: Vec<String> = drv_paths.iter().map(|p| drv_name(p)).collect();
    let tx = handles.db.web_db.inner().begin().await?;
    let commit = MCommit {
        id: CommitId::now_v7(),
        message: format!("SSH build of {}", names.join(" ")),
        hash: vec![0; 20],
        author: Some(session.user.id),
        author_name: session.user.name.clone(),
    }
    .into_active_model()
    .insert(&tx)
    .await?;

    let created = now();
    let evaluation = MEvaluation {
        id: EvaluationId::now_v7(),
        task: Some(task),
        repository: "ssh".into(),
        commit: commit.id,
        wildcard: names.join(" "),
        status: EvaluationStatus::EvaluatingDerivation,
        kind: EvaluationKind::Ssh,
        started_by: Some(session.user.id),
        concurrent: true,
        branch: gradient_types::branch_key::NO_BRANCH.to_owned(),
        created_at: created,
        updated_at: created,
        eval_drv_started_at: Some(created),
        ..Default::default()
    }
    .into_active_model()
    .insert(&tx)
    .await?;
    tx.commit().await?;

    Ok(evaluation.id)
}

async fn add(
    session: &Session,
    target: Target,
    closure: &[(String, Derivation)],
    drv_paths: &[String],
) -> anyhow::Result<Added> {
    let Target {
        task,
        evaluation,
        hold,
        ..
    } = target;
    let mut added = add_batches(session, task, evaluation, closure, drv_paths).await;
    if matches!(added, Ok(Added::Yes)) && ended(session, evaluation).await? {
        added = Ok(Added::EvaluationEnded);
    }
    drop(hold);

    if !matches!(added, Ok(Added::EvaluationEnded)) {
        session
            .handles
            .graph
            .transition(Transition::EvalStreamCompleted { evaluation })
            .await?;
    }
    added
}

async fn ended(session: &Session, evaluation: EvaluationId) -> anyhow::Result<bool> {
    Ok(
        by_id::<EEvaluation, _>(&session.handles.db.web_db, evaluation)
            .await?
            .is_none_or(|e| EvaluationStatus::TERMINAL.contains(&e.status)),
    )
}

async fn add_batches(
    session: &Session,
    task: TaskId,
    evaluation: EvaluationId,
    closure: &[(String, Derivation)],
    drv_paths: &[String],
) -> anyhow::Result<Added> {
    let handles = &session.handles;
    let derivations = discovered(closure, drv_paths)?;
    for batch in derivations.chunks(RECORD_BATCH_SIZE) {
        let report = handles
            .graph
            .record(RecordBatch {
                evaluation,
                task: Some(task),
                derivations: batch.to_vec(),
                warnings: vec![],
                errors: vec![],
            })
            .await?;
        if report.skipped {
            return Ok(Added::EvaluationEnded);
        }
    }

    Ok(Added::Yes)
}

fn discovered(
    closure: &[(String, Derivation)],
    drv_paths: &[String],
) -> anyhow::Result<Vec<DiscoveredDerivation>> {
    let requested: HashSet<&str> = drv_paths.iter().map(String::as_str).collect();
    closure
        .iter()
        .map(|(path, drv)| {
            let base = StorePath::parse_stored(path)?.base();
            let attr = requested
                .contains(path.as_str())
                .then(|| entry_point_name(&base));
            let mut discovered = discovered_derivation(attr, base, drv);
            for dependency in &mut discovered.dependencies {
                *dependency = StorePath::parse_stored(dependency)?.base();
            }

            Ok(discovered)
        })
        .collect()
}

// Entry points are unique per evaluation and name; many derivations share a name like `source`.
fn entry_point_name(base: &str) -> String {
    base.strip_suffix(".drv").unwrap_or(base).to_owned()
}

pub async fn run(
    session: &Session,
    drv_paths: &[String],
    log: impl Fn(LogMessage) + Send + Sync,
) -> anyhow::Result<BuildOutcome> {
    let started = start(session, drv_paths).await?;
    log(LogMessage::message(format!(
        "Gradient evaluation {}",
        started.evaluation
    )));
    wait(session, &started, log).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SshHandles;
    use gradient_derivation::DerivationOutput;
    use gradient_fixtures::mock::{empty_db, mock_db};
    use gradient_test_support::state::TestState;
    use sea_orm::{DbBackend, QueryTrait};
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
                .map(|i| gradient_derivation::InputDrv::new(path(i), vec!["out".into()]))
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
        let db = empty_db();
        let session = session(db.clone()).await;

        let e = start(&session, &[path("a")]).await.expect_err("refused");
        assert!(e.to_string().contains("TriggerEvaluation"), "{e}");
        assert!(db.into_transaction_log().is_empty());
    }

    async fn session(db: sea_orm::DatabaseConnection) -> Session {
        Session::open(
            SshHandles::for_tests(&TestState::new().web_db(db).build()),
            gradient_fixtures::rows::user(),
            gradient_fixtures::rows::project(),
            0,
            vec![],
        )
        .await
        .unwrap()
    }

    fn build_request_task() -> MTask {
        MTask {
            id: TaskId::now_v7(),
            project: gradient_fixtures::rows::project().id,
            name: gradient_db::build_request_task::BUILD_REQUEST_TASK_NAME.into(),
            managed: true,
            ..Default::default()
        }
    }

    fn ssh_evaluation(task: &MTask) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            task: Some(task.id),
            kind: EvaluationKind::Ssh,
            status: EvaluationStatus::Building,
            started_by: Some(gradient_fixtures::rows::user().id),
            ..Default::default()
        }
    }

    fn commit() -> MCommit {
        MCommit {
            id: CommitId::now_v7(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_second_request_joins_the_running_evaluation() {
        let task = build_request_task();
        let running = ssh_evaluation(&task);
        let db = mock_db()
            .append_query_results([vec![task.clone()]])
            .append_query_results([vec![running.clone()]])
            .into_connection();
        let session = session(db).await;

        let target = evaluation_for(&session, &[path("a")])
            .await
            .expect("target");

        assert_eq!(target.evaluation, running.id);
        assert!(
            session
                .handles
                .db
                .memory
                .held_evaluations
                .holds(running.id)
                .await
        );
        drop(target);
        assert!(
            !session
                .handles
                .db
                .memory
                .held_evaluations
                .holds(running.id)
                .await
        );
    }

    #[tokio::test]
    async fn a_request_without_a_running_evaluation_creates_an_evaluation() {
        let task = build_request_task();
        let created = ssh_evaluation(&task);
        let db = mock_db()
            .append_query_results([vec![task.clone()]])
            .append_query_results([Vec::<MEvaluation>::new()])
            .append_query_results([vec![commit()]])
            .append_query_results([vec![created.clone()]])
            .into_connection();
        let session = session(db).await;

        let target = evaluation_for(&session, &[path("a")])
            .await
            .expect("target");

        assert_eq!(target.evaluation, created.id);
        assert!(
            session
                .handles
                .db
                .memory
                .held_evaluations
                .holds(created.id)
                .await
        );
    }

    #[test]
    fn the_lookup_matches_task_kind_user_and_the_statuses_that_take_batches() {
        let task = TaskId::now_v7();
        let user = UserId::now_v7();

        let sql = running_evaluation(task, user)
            .build(DbBackend::Postgres)
            .to_string();

        assert!(sql.contains(&format!("\"task\" = '{task}'")), "{sql}");
        assert!(sql.contains(&format!("\"started_by\" = '{user}'")), "{sql}");
        assert!(sql.contains("\"kind\" ="), "{sql}");
        assert!(sql.contains("\"status\" IN (2, 3, 4)"), "{sql}");
    }

    #[test]
    fn requests_joining_one_evaluation_with_alike_named_derivations_keep_separate_entry_points() {
        let first = format!("/nix/store/{}-source.drv", "a".repeat(32));
        let second = format!("/nix/store/{}-source.drv", "b".repeat(32));

        let entry_point = |path: &String| {
            let found = discovered(&[(path.clone(), drv(&[]))], std::slice::from_ref(path))
                .expect("discovered");
            found[0].attr.clone()
        };

        assert_ne!(entry_point(&first), entry_point(&second));
    }

    #[test]
    fn drv_names_drop_the_hash_and_suffix() {
        assert_eq!(drv_name(&path("hello-2.12")), "hello-2.12");
    }
}
