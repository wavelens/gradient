/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod insert;
mod scheduling;

pub use scheduling::{eval_assignment, eval_job_key};

use chrono::{Duration, NaiveDateTime};
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::dispatched_job::DispatchedJobKind;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{
    BuildAttemptId, BuildJobId, CacheId, CachedPathId, DerivationBuildId, DerivationId,
    DerivationOutputId, DispatchedJobId, EntryPointId, EvaluationId, ProjectCacheId, ProjectId,
    TaskId, UserId,
};
use gradient_entity::{
    StorePath, build_attempt, build_job, cache, cache_upstream, cached_path, cluster_attempt,
    cluster_job, cluster_member, commit, derivation, derivation_build, derivation_dependency,
    derivation_feature, derivation_metric, derivation_output, dispatched_job, dispatched_job_phase,
    entry_point, eval_cache_store, eval_split, evaluation, evaluation_flake_input_override,
    evaluation_input_update, evaluation_metric, feature, project, project_cache, task, task_action,
    task_flake_input_override, task_trigger, user, worker_registration,
};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::database::TestDatabase;
use crate::{rows, test_date};

pub fn shape() -> Shape {
    Shape::default()
}

#[derive(Default)]
pub struct Shape {
    pub(super) owner: Option<user::Model>,
    pub(super) projects: Vec<project::Model>,
    pub(super) tasks: Vec<task::Model>,
    pub(super) commits: Vec<commit::Model>,
    pub(super) evaluations: Vec<evaluation::Model>,
    pub(super) derivations: Vec<derivation::Model>,
    pub(super) shared_builds: Vec<derivation_build::Model>,
    pub(super) edges: Vec<derivation_dependency::Model>,
    pub(super) outputs: Vec<derivation_output::Model>,
    pub(super) jobs: Vec<build_job::Model>,
    pub(super) entry_points: Vec<entry_point::Model>,
    pub(super) cached_paths: Vec<cached_path::Model>,
    pub(super) caches: Vec<cache::Model>,
    pub(super) project_caches: Vec<project_cache::Model>,
    pub(super) assignments: Vec<dispatched_job::Model>,
    pub(super) attempts: Vec<build_attempt::Model>,
    pub(super) triggers: Vec<task_trigger::Model>,
    pub(super) actions: Vec<task_action::Model>,
    pub(super) tracked_inputs: Vec<task_flake_input_override::Model>,
    pub(super) clusters: Vec<cluster_job::Model>,
    pub(super) members: Vec<cluster_member::Model>,
    pub(super) eval_splits: Vec<eval_split::Model>,
    pub(super) cluster_attempts: Vec<cluster_attempt::Model>,
    pub(super) phases: Vec<dispatched_job_phase::Model>,
    pub(super) features: Vec<feature::Model>,
    pub(super) derivation_features: Vec<derivation_feature::Model>,
    pub(super) upstreams: Vec<cache_upstream::Model>,
    pub(super) input_overrides: Vec<evaluation_flake_input_override::Model>,
    pub(super) input_updates: Vec<evaluation_input_update::Model>,
    pub(super) derivation_metrics: Vec<derivation_metric::Model>,
    pub(super) evaluation_metrics: Vec<evaluation_metric::Model>,
    pub(super) eval_caches: Vec<eval_cache_store::Model>,
    pub(super) registrations: Vec<worker_registration::Model>,
    task_of: Vec<usize>,
    evaluation: Option<usize>,
    derivation: Option<usize>,
    cache: Option<usize>,
    cached_path: Option<usize>,
    assignment: Option<usize>,
}

impl Shape {
    pub fn evaluation(mut self, id: EvaluationId) -> Self {
        if let Some(declared) = self.evaluations.iter().position(|e| e.id == id) {
            self.evaluation = Some(declared);
            return self;
        }
        self.owner();
        let slug = Uuid::now_v7().simple().to_string();
        let project = rows::project_with_id(ProjectId::now_v7(), &format!("project-{slug}"));
        let task = task::Model {
            id: TaskId::now_v7(),
            project: project.id,
            name: format!("task-{slug}"),
            retry_failed_builds: true,
            ..rows::task()
        };
        let commit = rows::build_request_commit();
        self.evaluations.push(evaluation::Model {
            id,
            status: EvaluationStatus::EvaluatingDerivation,
            ..rows::queued_evaluation(task.id, commit.id)
        });
        self.projects.push(project);
        self.tasks.push(task);
        self.commits.push(commit);
        self.task_of.push(self.tasks.len() - 1);
        self.evaluation = Some(self.evaluations.len() - 1);
        self
    }

    pub fn evaluation_status(mut self, status: EvaluationStatus) -> Self {
        let at = self.current_evaluation();
        self.evaluations[at].status = status;
        if status == EvaluationStatus::Building || status.is_terminal() {
            self.evaluations[at].building_started_at = Some(test_date());
        }
        self
    }

    pub fn same_task_as(mut self, evaluation: EvaluationId) -> Self {
        let at = self.current_evaluation();
        let shared = self.task_of[self.evaluation_at(evaluation)];
        let own = std::mem::replace(&mut self.task_of[at], shared);
        self.evaluations[at].task = Some(self.tasks[shared].id);
        self.drop_unused_task(own);
        self
    }

    pub fn task_option(mut self, change: impl FnOnce(&mut task::Model)) -> Self {
        let at = self.current_task();
        change(&mut self.tasks[at]);
        self
    }

    pub fn without_cache_signing(self) -> Self {
        self.task_option(|task| task.sign_cache = false)
    }

    pub fn derivation(mut self, path: &str) -> Self {
        self.derivation = Some(self.derivation_at(path));
        self
    }

    pub fn walked(mut self) -> Self {
        let at = self.current_derivation();
        self.derivations[at].walked = true;
        self
    }

    pub fn depends_on(self, path: &str) -> Self {
        self.edge(path, EdgeKind::Buildtime)
    }

    pub fn runtime_depends_on(self, path: &str) -> Self {
        self.edge(path, EdgeKind::Runtime)
    }

    pub fn both_depends_on(self, path: &str) -> Self {
        self.edge(path, EdgeKind::Both)
    }

    pub fn ifd(mut self) -> Self {
        let at = self.current_derivation();
        self.derivations[at].ifd = true;
        self
    }

    pub fn created_at(mut self, at: NaiveDateTime) -> Self {
        let current = self.current_derivation();
        let derivation = &mut self.derivations[current];
        derivation.created_at = at;
        let id = derivation.id;
        let mut hashes = vec![derivation.hash.clone()];
        for output in self.outputs.iter_mut().filter(|o| o.derivation == id) {
            output.created_at = at;
            hashes.extend(output.hash.clone());
        }
        for path in &mut self.cached_paths {
            if hashes.contains(&path.hash) {
                path.created_at = at;
            }
        }
        self
    }

    pub fn output(self, hash: &str) -> Self {
        self.named_output(hash, "out")
    }

    pub fn named_output(mut self, hash: &str, name: &str) -> Self {
        let at = self.current_derivation();
        let derivation = self.derivations[at].id;
        let declared = self
            .outputs
            .iter()
            .find(|o| o.derivation == derivation && o.hash.as_deref() == Some(hash));
        if let Some(declared) = declared {
            assert_eq!(
                declared.name, name,
                "output {hash}: a derivation names a hash in a single output"
            );
        } else {
            let mut output = derivation_output::Model {
                id: DerivationOutputId::now_v7(),
                derivation,
                name: name.into(),
                hash: Some(hash.into()),
                package: Some(output_package(&self.derivations[at].name, name)),
                created_at: test_date(),
                ..Default::default()
            };
            if self.shared_builds[at].cache_available {
                known_upstream(&mut output);
            }
            self.outputs.push(output);
        }
        self
    }

    pub fn job(mut self) -> Self {
        let (evaluation, at) = (self.current_evaluation(), self.current_derivation());
        if self.job_of(evaluation, at).is_none() {
            self.jobs.push(build_job::Model {
                id: BuildJobId::now_v7(),
                evaluation: self.evaluations[evaluation].id,
                derivation: self.derivations[at].id,
                derivation_build: self.shared_builds[at].id,
                score_breakdown: serde_json::json!({}),
                created_at: test_date(),
                ..Default::default()
            });
        }
        self
    }

    pub fn entry_point(mut self, attr: &str) -> Self {
        let (evaluation, derivation, task) = (
            self.evaluations[self.current_evaluation()].id,
            self.derivations[self.current_derivation()].id,
            self.tasks[self.current_task()].id,
        );
        match self
            .entry_points
            .iter_mut()
            .find(|p| p.evaluation == evaluation && p.eval == attr)
        {
            Some(declared) => declared.derivation = derivation,
            None => self.entry_points.push(entry_point::Model {
                id: EntryPointId::now_v7(),
                task,
                evaluation,
                derivation,
                eval: attr.into(),
                created_at: test_date(),
                ..Default::default()
            }),
        }
        self
    }

    pub fn build_status(mut self, status: BuildStatus) -> Self {
        let at = self.current_derivation();
        self.shared_builds[at].status = status;
        self
    }

    pub fn probed(mut self) -> Self {
        let at = self.current_derivation();
        self.shared_builds[at].probed = true;
        self.shared_builds[at].probed_at = Some(chrono::Utc::now().naive_utc());
        self
    }

    pub fn cache_available(mut self) -> Self {
        let at = self.current_derivation();
        self.shared_builds[at].cache_available = true;
        let derivation = self.derivations[at].id;
        self.outputs
            .iter_mut()
            .filter(|o| o.derivation == derivation)
            .for_each(known_upstream);
        self
    }

    pub fn attempt(
        mut self,
        outcome: AttemptOutcome,
        reason: Option<AttemptFailureReason>,
    ) -> Self {
        let at = self.current_derivation();
        let shared_build = self.shared_builds[at].id;
        let build_job = self
            .evaluation
            .and_then(|evaluation| self.job_of(evaluation, at))
            .map(|job| job.id);
        let running = outcome == AttemptOutcome::Running;
        let dispatched_job = self.assignment_of(shared_build).filter(|_| running);
        let created_at = test_date() + Duration::seconds(self.attempts.len() as i64);
        self.attempts.push(build_attempt::Model {
            id: BuildAttemptId::now_v7(),
            build_job,
            derivation_build: shared_build,
            dispatched_job,
            outcome,
            reason,
            build_context: serde_json::json!({}),
            build_finished_at: (!running).then_some(created_at),
            created_at,
            ..Default::default()
        });
        self
    }

    pub fn open_assignment(mut self) -> Self {
        let (evaluation, at) = (self.current_evaluation(), self.current_derivation());
        let shared_build = self.shared_builds[at].id;
        let key = build_job_key(shared_build);
        if let Some(declared) = self
            .assignments
            .iter()
            .position(|a| a.job_id.as_deref() == Some(key.as_str()))
        {
            self.assignment = Some(declared);
        } else {
            let task = self.current_task();
            let assignment = build_assignment(
                self.evaluations[evaluation].id,
                self.projects[task].id,
                self.tasks[task].id,
                shared_build,
                test_date(),
            );
            if let Some(latest) = self
                .attempts
                .iter_mut()
                .rev()
                .find(|a| a.derivation_build == shared_build)
                .filter(|a| a.outcome == AttemptOutcome::Running)
            {
                latest.dispatched_job = Some(assignment.id);
            }
            self.assignments.push(assignment);
            self.assignment = Some(self.assignments.len() - 1);
        }
        self
    }

    pub fn drv_cached(mut self) -> Self {
        let at = self.current_derivation();
        let hash = self.derivations[at].hash.clone();
        let package = format!("{}.drv", self.derivations[at].name);
        self.stored_path(&hash, &package);
        self
    }

    pub fn cached_output(mut self, path: &str) -> Self {
        let at = self.derivation_at(path);
        let derivation = self.derivations[at].id;
        let output = self
            .outputs
            .iter()
            .position(|o| o.derivation == derivation)
            .unwrap_or_else(|| panic!("cached output of {path}: declare its output first"));
        let hash = self.outputs[output]
            .hash
            .clone()
            .expect("a realised output");
        let package = self.outputs[output]
            .package
            .clone()
            .expect("a realised output");
        self.cached_path = Some(self.stored_path(&hash, &package));
        self
    }

    pub fn cached_path(mut self, hash: &str, package: &str) -> Self {
        self.cached_path = Some(self.stored_path(hash, package));
        self
    }

    pub fn references(mut self, paths: &[(&str, &str)]) -> Self {
        let at = self.current_cached_path();
        let stored: Vec<String> = paths
            .iter()
            .map(|(hash, package)| format!("{hash}-{package}"))
            .collect();
        self.cached_paths[at].references = Some(stored.join(" "));
        self
    }

    pub fn unconfirmed(mut self) -> Self {
        let at = self.current_cached_path();
        self.cached_paths[at].confirmed = false;
        self
    }

    pub fn into_cache(mut self, id: CacheId) -> Self {
        let owner = self.owner();
        let project = self.projects[self.current_task()].id;
        self.cache = Some(self.cache_at(id, owner));
        let linked = self
            .project_caches
            .iter()
            .any(|l| l.project == project && l.cache == id);
        if !linked {
            self.project_caches.push(project_cache::Model {
                id: ProjectCacheId::now_v7(),
                project,
                cache: id,
                ..Default::default()
            });
        }
        self
    }

    pub fn keyed(mut self, private_key: impl Into<String>) -> Self {
        let at = self.cache.expect("name a cache first");
        self.caches[at].private_key = private_key.into();
        self
    }

    pub fn declared_statuses(&self) -> Vec<(String, BuildStatus)> {
        self.derivations
            .iter()
            .zip(&self.shared_builds)
            .map(|(d, b)| (format!("{}-{}.drv", d.hash, d.name), b.status))
            .collect()
    }

    pub async fn into_database(self) -> TestDatabase {
        let db = TestDatabase::create().await;
        self.insert_into(&db.connection()).await;
        db
    }

    pub async fn insert_into(mut self, db: &DatabaseConnection) {
        self.open_member_rows();
        self.attempts_follow_the_builds();
        insert::all(self, db).await;
    }

    fn attempts_follow_the_builds(&mut self) {
        for attempt in &mut self.attempts {
            let build = self
                .shared_builds
                .iter()
                .find(|b| b.id == attempt.derivation_build)
                .expect("an attempt is declared on a build");
            attempt.substitute = build.cache_available;
            if attempt.outcome == AttemptOutcome::Running && build.status == BuildStatus::Building {
                attempt.build_started_at = Some(attempt.created_at);
            }
        }
    }

    fn owner(&mut self) -> UserId {
        self.owner.get_or_insert_with(rows::user).id
    }

    fn current_evaluation(&self) -> usize {
        self.evaluation.expect("name an evaluation first")
    }

    fn current_derivation(&self) -> usize {
        self.derivation.expect("name a derivation first")
    }

    fn current_task(&self) -> usize {
        self.task_of[self.current_evaluation()]
    }

    fn current_cached_path(&self) -> usize {
        self.cached_path.expect("name a cached path first")
    }

    fn evaluation_at(&self, id: EvaluationId) -> usize {
        self.evaluations
            .iter()
            .position(|e| e.id == id)
            .unwrap_or_else(|| panic!("declare evaluation {id} first"))
    }

    fn drop_unused_task(&mut self, at: usize) {
        if self.task_of.contains(&at) {
            return;
        }
        self.tasks.remove(at);
        self.projects.remove(at);
        for index in self.task_of.iter_mut().filter(|index| **index > at) {
            *index -= 1;
        }
    }

    fn job_of(&self, evaluation: usize, derivation: usize) -> Option<&build_job::Model> {
        let (evaluation, derivation) = (
            self.evaluations[evaluation].id,
            self.derivations[derivation].id,
        );
        self.jobs
            .iter()
            .find(|j| j.evaluation == evaluation && j.derivation == derivation)
    }

    fn assignment_of(&self, shared_build: DerivationBuildId) -> Option<DispatchedJobId> {
        let key = build_job_key(shared_build);
        self.assignments
            .iter()
            .find(|a| a.job_id.as_deref() == Some(key.as_str()))
            .map(|a| a.id)
    }

    fn derivation_at(&mut self, path: &str) -> usize {
        let (hash, name) = drv_hash_name(path);
        if let Some(at) = self
            .derivations
            .iter()
            .position(|d| d.hash == hash && d.name == name)
        {
            return at;
        }
        let id = DerivationId::now_v7();
        self.derivations.push(derivation::Model {
            id,
            hash,
            name,
            architecture: "x86_64-linux".into(),
            created_at: test_date(),
            ..Default::default()
        });
        self.shared_builds.push(derivation_build::Model {
            id: DerivationBuildId::now_v7(),
            derivation: id,
            created_at: test_date(),
            updated_at: test_date(),
            ..Default::default()
        });
        self.derivations.len() - 1
    }

    fn cache_at(&mut self, id: CacheId, owner: UserId) -> usize {
        if let Some(at) = self.caches.iter().position(|c| c.id == id) {
            return at;
        }
        self.caches.push(cache::Model {
            private_key: String::new(),
            ..rows::cache_with_id(id, &format!("cache-{}", id.into_inner().simple()), owner)
        });
        self.caches.len() - 1
    }

    fn edge(mut self, path: &str, kind: EdgeKind) -> Self {
        let derivation = self.derivations[self.current_derivation()].id;
        let to = self.derivation_at(path);
        let dependency = self.derivations[to].id;
        match self
            .edges
            .iter_mut()
            .find(|e| e.derivation == derivation && e.dependency == dependency)
        {
            Some(declared) => declared.kind = kind,
            None => self.edges.push(derivation_dependency::Model {
                derivation,
                dependency,
                kind,
            }),
        }
        self
    }

    fn stored_path(&mut self, hash: &str, package: &str) -> usize {
        if let Some(declared) = self.cached_paths.iter().position(|p| p.hash == hash) {
            return declared;
        }
        self.cached_paths.push(cached_path::Model {
            id: CachedPathId::now_v7(),
            hash: hash.into(),
            package: package.into(),
            file_hash: Some("sha256:abc".into()),
            file_size: Some(5),
            nar_size: Some(5),
            nar_hash: Some("sha256:def".into()),
            confirmed: true,
            created_at: test_date(),
            ..Default::default()
        });
        self.cached_paths.len() - 1
    }
}

pub fn build_job_key(shared_build: DerivationBuildId) -> String {
    format!("build:{shared_build}")
}

pub fn build_assignment(
    evaluation: EvaluationId,
    project: ProjectId,
    task: TaskId,
    shared_build: DerivationBuildId,
    at: NaiveDateTime,
) -> dispatched_job::Model {
    dispatched_job::Model {
        id: DispatchedJobId::now_v7(),
        kind: DispatchedJobKind::Build,
        evaluation_id: evaluation,
        project,
        task: Some(task),
        worker_id: "worker".into(),
        job_id: Some(build_job_key(shared_build)),
        queued_at: at,
        dispatched_at: at,
        score_breakdown: serde_json::json!({}),
        worker_context: serde_json::json!({}),
        job_context: serde_json::json!({}),
        created_at: at,
        ..Default::default()
    }
}

fn output_package(derivation: &str, output: &str) -> String {
    match output {
        "out" => derivation.to_owned(),
        named => format!("{derivation}-{named}"),
    }
}

fn known_upstream(output: &mut derivation_output::Model) {
    output.external_url = Some(format!(
        "https://upstream.invalid/nar/{}.nar",
        output.hash.as_deref().unwrap_or_default()
    ));
    output.nar_hash = Some("sha256:def".into());
    output.nar_size = Some(5);
}

pub(crate) fn drv_hash_name(path: &str) -> (String, String) {
    let parsed =
        StorePath::parse_stored(path).unwrap_or_else(|_| panic!("{path} is no store path"));
    let name = parsed
        .name()
        .strip_suffix(".drv")
        .unwrap_or_else(|| panic!("{path} is no drv path"));
    (parsed.hash().to_owned(), name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::{CacheId, EvaluationId};
    use gradient_entity::{cache, derivation, project, project_cache};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";
    const A_OUT: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const B_OUT: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";
    const A_DEV: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

    #[tokio::test]
    async fn a_shape_writes_every_table_the_claim_statement_joins() {
        let (evaluation, cache) = (EvaluationId::now_v7(), CacheId::now_v7());
        let db = shape()
            .evaluation(evaluation)
            .derivation(A)
            .walked()
            .depends_on(B)
            .derivation(B)
            .output(B_OUT)
            .job()
            .cached_output(B)
            .into_cache(cache)
            .keyed("key")
            .into_database()
            .await;

        assert!(db.derivation(A).await.walked);
        assert!(!db.derivation(B).await.walked);
        assert_eq!(
            db.edge(A, B).await.expect("the edge").kind,
            EdgeKind::Buildtime
        );
        let stored = db.cached_path(B_OUT).await;
        assert!(
            stored.as_ref().is_some_and(|p| p.file_hash.is_some()),
            "{stored:?}"
        );
        let caches = db.rows::<cache::Entity>().await;
        assert!(matches!(caches.as_slice(), [c] if c.id == cache && c.private_key == "key"));
        let project = db.project_of(evaluation).await;
        let links = db.rows::<project_cache::Entity>().await;
        assert!(matches!(links.as_slice(), [l] if l.project == project));
        db.finish().await;
    }

    #[tokio::test]
    async fn naming_a_derivation_twice_keeps_a_single_row() {
        let db = shape()
            .derivation(A)
            .depends_on(B)
            .derivation(B)
            .output(B_OUT)
            .derivation(A)
            .walked()
            .cached_output(B)
            .cached_output(B)
            .into_database()
            .await;

        assert_eq!(db.rows::<derivation::Entity>().await.len(), 2);
        assert_eq!(
            db.rows::<gradient_entity::cached_path::Entity>()
                .await
                .len(),
            1
        );
        assert!(db.derivation(A).await.walked);
        db.finish().await;
    }

    #[tokio::test]
    async fn a_repeated_output_or_edge_stays_a_single_row() {
        let db = shape()
            .derivation(A)
            .output(A_OUT)
            .output(A_OUT)
            .depends_on(B)
            .runtime_depends_on(B)
            .into_database()
            .await;

        assert_eq!(db.rows::<derivation_output::Entity>().await.len(), 1);
        let edges = db.rows::<derivation_dependency::Entity>().await;
        assert!(
            matches!(edges.as_slice(), [edge] if edge.kind == EdgeKind::Runtime),
            "{edges:?}"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn two_evaluations_get_two_projects() {
        let (first, second) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = shape()
            .evaluation(first)
            .evaluation(second)
            .into_database()
            .await;

        assert_eq!(db.rows::<project::Entity>().await.len(), 2);
        assert_ne!(db.project_of(first).await, db.project_of(second).await);
        db.finish().await;
    }

    #[tokio::test]
    async fn an_evaluation_of_the_same_task_shares_its_project() {
        let (first, second) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = shape()
            .evaluation(first)
            .evaluation_status(EvaluationStatus::Failed)
            .evaluation(second)
            .same_task_as(first)
            .derivation(A)
            .entry_point("pkg.a")
            .into_database()
            .await;

        let shared = db.task_of(first).await;
        assert_eq!(db.task_of(second).await, shared);
        assert_eq!(db.rows::<task::Entity>().await.len(), 1);
        assert_eq!(db.rows::<project::Entity>().await.len(), 1);
        let point = db
            .entry_point(second, "pkg.a")
            .await
            .expect("the entry point");
        assert_eq!(point.task, shared);
        db.finish().await;
    }

    #[tokio::test]
    async fn a_named_output_is_a_second_row_of_its_derivation() {
        let db = shape()
            .derivation(A)
            .cache_available()
            .output(A_OUT)
            .named_output(A_DEV, "dev")
            .into_database()
            .await;

        let (out, dev) = (db.output_of(A, "out").await, db.output_of(A, "dev").await);
        assert_eq!(
            (out.hash.as_deref(), out.package.as_deref()),
            (Some(A_OUT), Some("a"))
        );
        assert_eq!(
            (dev.hash.as_deref(), dev.package.as_deref()),
            (Some(A_DEV), Some("a-dev"))
        );
        assert!(out.external_url.is_some() && dev.external_url.is_some());
        db.finish().await;
    }

    #[tokio::test]
    async fn an_open_assignment_belongs_to_the_running_attempt_of_its_build() {
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .derivation(A)
            .job()
            .attempt(
                AttemptOutcome::Failed,
                Some(AttemptFailureReason::BuilderNonzero),
            )
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .derivation(B)
            .attempt(AttemptOutcome::Running, None)
            .into_database()
            .await;

        let job = db.job(evaluation, A).await.expect("the job of A").id;
        let assignment = db.open_assignment(A).await.expect("the assignment of A");
        let attempts = db.attempts(A).await;
        let [failed, running] = attempts.as_slice() else {
            panic!("two attempts of A, oldest first: {attempts:?}");
        };
        assert_eq!(failed.outcome, AttemptOutcome::Failed);
        assert_eq!(failed.reason, Some(AttemptFailureReason::BuilderNonzero));
        assert_eq!((failed.build_job, failed.dispatched_job), (Some(job), None));
        assert_eq!(running.outcome, AttemptOutcome::Running);
        assert_eq!(
            (running.build_job, running.dispatched_job),
            (Some(job), Some(assignment.id))
        );
        assert_eq!(assignment.evaluation_id, evaluation);
        assert!(db.open_assignment(B).await.is_none());
        assert_eq!(db.attempts(B).await[0].dispatched_job, None);
        db.finish().await;
    }

    #[tokio::test]
    async fn a_running_attempt_declared_after_the_assignment_points_at_it() {
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(A)
            .open_assignment()
            .attempt(AttemptOutcome::Running, None)
            .cache_available()
            .build_status(BuildStatus::Building)
            .into_database()
            .await;

        let assignment = db.open_assignment(A).await.expect("the assignment of A");
        let attempts = db.attempts(A).await;
        let [running] = attempts.as_slice() else {
            panic!("a single attempt of A: {attempts:?}");
        };
        assert_eq!(running.dispatched_job, Some(assignment.id));
        assert!(running.substitute, "{running:?}");
        assert_eq!(running.build_started_at, Some(running.created_at));
        db.finish().await;
    }

    #[test]
    #[should_panic(expected = "names a hash in a single output")]
    fn a_hash_declared_under_a_second_output_name_panics() {
        let _ = shape()
            .derivation(A)
            .output(A_OUT)
            .named_output(A_OUT, "dev");
    }

    #[tokio::test]
    async fn references_and_unconfirmed_change_the_cached_output_only() {
        let db = shape()
            .derivation(A)
            .drv_cached()
            .output(A_OUT)
            .cached_output(A)
            .unconfirmed()
            .references(&[(A_OUT, "a"), (B_OUT, "b")])
            .into_database()
            .await;

        let output = db.cached_path(A_OUT).await.expect("the output path");
        assert!(
            !output.confirmed && output.file_hash.is_some(),
            "{output:?}"
        );
        assert_eq!(
            output.references,
            Some(format!("{A_OUT}-a {B_OUT}-b")),
            "{output:?}"
        );
        let (drv_hash, _) = drv_hash_name(A);
        let drv = db.cached_path(&drv_hash).await.expect("the drv path");
        assert!(drv.confirmed && drv.references.is_none(), "{drv:?}");
        db.finish().await;
    }

    #[tokio::test]
    async fn created_at_moves_a_derivation_with_its_outputs_and_cached_paths() {
        let earlier = test_date() - Duration::days(2);
        let db = shape()
            .derivation(A)
            .drv_cached()
            .output(A_OUT)
            .cached_output(A)
            .created_at(earlier)
            .derivation(B)
            .drv_cached()
            .output(B_OUT)
            .cached_output(B)
            .into_database()
            .await;

        assert_eq!(db.derivation(A).await.created_at, earlier);
        assert_eq!(db.output(A_OUT).await.created_at, earlier);
        let moved = db.cached_path(A_OUT).await.expect("the output path of A");
        assert_eq!(moved.created_at, earlier);
        let (drv_hash, _) = drv_hash_name(A);
        let drv = db.cached_path(&drv_hash).await.expect("the drv path of A");
        assert_eq!(drv.created_at, earlier);
        assert_eq!(db.derivation(B).await.created_at, test_date());
        assert_eq!(db.output(B_OUT).await.created_at, test_date());
        let other = db.cached_path(B_OUT).await.expect("the output path of B");
        assert_eq!(other.created_at, test_date());
        db.finish().await;
    }

    #[test]
    #[should_panic(expected = "name a derivation first")]
    fn a_setter_without_a_named_derivation_panics() {
        let _ = shape().walked();
    }

    #[test]
    #[should_panic(expected = "not-a-store-path")]
    fn a_derivation_named_by_no_store_path_panics() {
        let _ = shape().derivation("not-a-store-path");
    }

    #[test]
    #[should_panic(expected = "declare its output first")]
    fn a_cached_output_needs_a_declared_output() {
        let _ = shape().derivation(A).cached_output(A);
    }

    mod scheduling {
        use gradient_entity::build_attempt::AttemptOutcome;
        use gradient_entity::cache_upstream::CacheUpstreamKind;
        use gradient_entity::cluster_job::ClusterJobStatus;
        use gradient_entity::dispatched_job::DispatchedJobOutcome;
        use gradient_entity::evaluation::EvaluationKind;
        use gradient_entity::ids::{
            CacheId, CacheUpstreamId, ClusterAttemptId, ClusterJobId, EvaluationId, TaskTriggerId,
        };
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_entity::task_trigger::TriggerType;
        use gradient_entity::{
            cache_upstream, cluster_attempt, cluster_job, cluster_member, derivation_feature,
            derivation_metric, dispatched_job, dispatched_job_phase, eval_cache_store, evaluation,
            evaluation_flake_input_override, evaluation_input_update, evaluation_metric, feature,
            task, task_trigger, worker_registration,
        };

        use super::{A, A_OUT, B};
        use crate::shape::{build_job_key, eval_job_key, shape};
        use crate::test_date;

        #[tokio::test]
        async fn evaluation_fields_and_a_trigger_go_to_the_evaluation_and_task_rows() {
            let (e, trigger) = (EvaluationId::now_v7(), TaskTriggerId::now_v7());
            let earlier = test_date() - chrono::Duration::hours(1);
            let db = shape()
                .evaluation(e)
                .repository("file:///repo")
                .commit_hash(&[7; 20])
                .wildcard("pkg.*")
                .evaluation_kind(EvaluationKind::InputUpdate)
                .waiting_reason(serde_json::json!({"kind": "approval"}))
                .updated_at(earlier)
                .last_evaluation(e)
                .trigger(
                    trigger,
                    TriggerType::Polling,
                    serde_json::json!({"interval_secs": 60}),
                    true,
                    Some(earlier),
                )
                .input_override("nixpkgs", "github:a/b")
                .input_update("c0", "g", &["nixpkgs"], true)
                .worker_registration("w1")
                .evaluation_metric(|m| m.peak_rss_mb = 512)
                .into_database()
                .await;

            let row = db
                .find::<evaluation::Entity>(e)
                .await
                .expect("the evaluation");
            assert_eq!(
                (row.repository.as_str(), row.wildcard.as_str(), row.kind),
                ("file:///repo", "pkg.*", EvaluationKind::InputUpdate)
            );
            assert_eq!(row.updated_at, earlier);
            assert!(row.waiting_reason.is_some());
            let commit = db
                .find::<gradient_entity::commit::Entity>(row.commit)
                .await
                .expect("the commit");
            assert_eq!(commit.hash, vec![7; 20]);
            let task = db
                .find::<task::Entity>(db.task_of(e).await)
                .await
                .expect("the task");
            assert_eq!(
                (task.repository.as_str(), task.last_evaluation),
                ("file:///repo", Some(e))
            );
            let triggers = db.rows::<task_trigger::Entity>().await;
            assert!(
                matches!(triggers.as_slice(), [t] if t.id == trigger && t.task == task.id && t.last_fired_at == Some(earlier)),
                "{triggers:?}"
            );
            let overrides = db.rows::<evaluation_flake_input_override::Entity>().await;
            assert!(
                matches!(overrides.as_slice(), [o] if o.evaluation == e && o.input_name == "nixpkgs")
            );
            let updates = db.rows::<evaluation_input_update::Entity>().await;
            assert!(
                matches!(updates.as_slice(), [u] if u.evaluation == e && u.discover_only && u.base_commit == "c0")
            );
            let registrations = db.rows::<worker_registration::Entity>().await;
            assert!(
                matches!(registrations.as_slice(), [r] if r.peer_id == task.project && r.enable_eval)
            );
            let metrics = db.rows::<evaluation_metric::Entity>().await;
            assert!(matches!(metrics.as_slice(), [m] if m.evaluation == e && m.peak_rss_mb == 512));
            db.finish().await;
        }

        #[tokio::test]
        async fn derivation_columns_features_and_metrics_are_written() {
            let db = shape()
                .evaluation(EvaluationId::now_v7())
                .derivation(A)
                .system("builtin")
                .fixed_output()
                .pname("hello")
                .feature("kvm")
                .derivation(B)
                .feature("kvm")
                .derivation(A)
                .derivation_metric(|m| m.peak_ram_mb = Some(100))
                .into_database()
                .await;

            let a = db.derivation(A).await;
            assert_eq!(
                (
                    a.architecture.as_str(),
                    a.is_fixed_output,
                    a.pname.as_deref()
                ),
                ("builtin", true, Some("hello"))
            );
            let features = db.rows::<feature::Entity>().await;
            assert!(
                matches!(features.as_slice(), [f] if f.name == "kvm"),
                "{features:?}"
            );
            assert_eq!(db.rows::<derivation_feature::Entity>().await.len(), 2);
            let metrics = db.rows::<derivation_metric::Entity>().await;
            assert!(
                matches!(metrics.as_slice(), [m] if m.derivation == a.id && m.pname.as_deref() == Some("hello") && m.architecture == "builtin" && m.peak_ram_mb == Some(100)),
                "{metrics:?}"
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn assignment_rows_take_the_worker_times_outcome_and_phases() {
            let e = EvaluationId::now_v7();
            let at = test_date() - chrono::Duration::hours(1);
            let db = shape()
                .evaluation(e)
                .eval_assignment()
                .by_worker("w1")
                .dispatched_at(at)
                .closed(DispatchedJobOutcome::Completed, at)
                .worker_elapsed_ms(900)
                .phase(0, 0, 100)
                .phase(2, 100, 200)
                .eval_assignment()
                .derivation(A)
                .job()
                .open_assignment()
                .ready_at(at)
                .into_database()
                .await;

            let rows = db.rows::<dispatched_job::Entity>().await;
            let key = eval_job_key(e);
            let mut evals: Vec<_> = rows
                .iter()
                .filter(|r| r.job_id.as_deref() == Some(key.as_str()))
                .collect();
            evals.sort_by_key(|r| r.created_at);
            let [closed, open] = evals.as_slice() else {
                panic!("two eval rows: {rows:?}");
            };
            assert_eq!(
                (
                    closed.worker_id.as_str(),
                    closed.dispatched_at,
                    closed.worker_elapsed_ms
                ),
                ("w1", at, Some(900))
            );
            assert_eq!(closed.outcome, Some(DispatchedJobOutcome::Completed));
            assert!(open.finished_at.is_none());
            let mut phases = db.rows::<dispatched_job_phase::Entity>().await;
            phases.sort_by_key(|p| p.seq);
            assert!(
                matches!(phases.as_slice(), [p0, p1] if p0.dispatched_job == closed.id && p1.seq == 1 && p1.phase == 2),
                "{phases:?}"
            );
            let build = db.open_assignment(A).await.expect("the build row");
            assert_eq!(build.ready_at, Some(at));
            db.finish().await;
        }

        #[tokio::test]
        async fn a_cluster_with_an_open_attempt_opens_a_row_per_member() {
            let (e1, e2) = (EvaluationId::now_v7(), EvaluationId::now_v7());
            let (cluster, attempt) = (ClusterJobId::now_v7(), ClusterAttemptId::now_v7());
            let db = shape()
                .evaluation(e1)
                .evaluation_member_of(cluster, "head", true, None)
                .evaluation(e2)
                .derivation(A)
                .job()
                .output(A_OUT)
                .build_member_of(cluster, "node", false, Some("w5"))
                .cluster(cluster, false, 2, ClusterJobStatus::Running, 1, test_date())
                .cluster_attempt(cluster, attempt, Some(test_date()), None, None)
                .into_database()
                .await;

            let clusters = db.rows::<cluster_job::Entity>().await;
            assert!(matches!(clusters.as_slice(), [c] if c.retry_budget == 2 && c.attempts == 1));
            let members = db.rows::<cluster_member::Entity>().await;
            assert_eq!(members.len(), 2);
            assert!(
                members
                    .iter()
                    .any(|m| m.evaluation == Some(e1) && m.primary)
            );
            assert!(members.iter().any(|m| m.pin.as_deref() == Some("w5")));
            let attempts = db.rows::<cluster_attempt::Entity>().await;
            assert!(
                matches!(attempts.as_slice(), [a] if a.id == attempt && a.finished_at.is_none())
            );
            let rows = db.rows::<dispatched_job::Entity>().await;
            let build_key = build_job_key(db.shared_build(A).await.id);
            assert!(rows.iter().all(|r| r.cluster_attempt == Some(attempt)));
            assert!(rows.iter().any(|r| r.job_id == Some(eval_job_key(e1))));
            assert!(
                rows.iter()
                    .any(|r| r.job_id == Some(build_key.clone()) && r.evaluation_id == e2)
            );
            assert_ne!(rows[0].worker_id, rows[1].worker_id);
            let build_row = rows
                .iter()
                .find(|r| r.job_id == Some(build_key.clone()))
                .expect("the build member row");
            let running = db.attempts(A).await;
            assert!(
                matches!(running.as_slice(), [a] if a.outcome == AttemptOutcome::Running && a.dispatched_job == Some(build_row.id)),
                "{running:?}"
            );
            db.finish().await;
        }

        #[tokio::test]
        async fn an_upstream_and_an_eval_cache_are_written() {
            let (cache, upstream) = (CacheId::now_v7(), CacheUpstreamId::now_v7());
            let db = shape()
                .evaluation(EvaluationId::now_v7())
                .into_cache(cache)
                .upstream(
                    upstream,
                    "http://127.0.0.1:1",
                    "up-1:key",
                    CacheUpstreamKind::Http,
                    CacheSubscriptionMode::ReadOnly,
                    false,
                )
                .eval_cache("fingerprint")
                .into_database()
                .await;

            let upstreams = db.rows::<cache_upstream::Entity>().await;
            assert!(
                matches!(upstreams.as_slice(), [u] if u.id == upstream && u.cache == cache && !u.active && u.mode == CacheSubscriptionMode::ReadOnly),
                "{upstreams:?}"
            );
            let caches = db.rows::<eval_cache_store::Entity>().await;
            assert!(matches!(caches.as_slice(), [c] if c.fingerprint == "fingerprint"));
            db.finish().await;
        }

        #[tokio::test]
        async fn a_second_shape_joins_a_live_database() {
            let (first, second) = (EvaluationId::now_v7(), EvaluationId::now_v7());
            let db = shape().evaluation(first).into_database().await;

            shape()
                .evaluation(second)
                .without_task()
                .insert_into(&db.connection())
                .await;

            let row = db
                .find::<evaluation::Entity>(second)
                .await
                .expect("the evaluation");
            assert_eq!(row.task, None);
            assert_eq!(db.rows::<evaluation::Entity>().await.len(), 2);
            db.finish().await;
        }
    }
}
