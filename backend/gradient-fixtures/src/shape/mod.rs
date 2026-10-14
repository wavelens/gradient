/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod insert;

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
    StorePath, build_attempt, build_job, cache, cached_path, commit, derivation, derivation_build,
    derivation_dependency, derivation_output, dispatched_job, entry_point, evaluation, project,
    project_cache, task, user,
};
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
    task_of: Vec<usize>,
    evaluation: Option<usize>,
    derivation: Option<usize>,
    cache: Option<usize>,
    cached_path: Option<usize>,
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
            hashes.push(output.hash.clone());
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
            .find(|o| o.derivation == derivation && o.hash == hash);
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
                hash: hash.into(),
                package: output_package(&self.derivations[at].name, name),
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
        if self.assignment_of(shared_build).is_none() {
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
        let hash = self.outputs[output].hash.clone();
        let package = self.outputs[output].package.clone();
        let stored = self.stored_path(&hash, &package);
        self.outputs[output].is_cached = true;
        self.outputs[output].cached_path = Some(self.cached_paths[stored].id);
        self.cached_path = Some(stored);
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

    pub async fn into_database(mut self) -> TestDatabase {
        self.attempts_follow_the_builds();
        let db = TestDatabase::create().await;
        insert::all(self, &db.connection()).await;
        db
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
    output.external_url = Some(format!("https://upstream.invalid/nar/{}.nar", output.hash));
    output.nar_hash = Some("sha256:def".into());
    output.nar_size = Some(5);
}

pub(crate) fn drv_hash_name(path: &str) -> (String, String) {
    let parsed = StorePath::parse(path).unwrap_or_else(|_| panic!("{path} is no store path"));
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
        let output = db.output(B_OUT).await;
        assert!(
            output.is_cached && output.cached_path.is_some(),
            "{output:?}"
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
        assert_eq!((out.hash.as_str(), out.package.as_str()), (A_OUT, "a"));
        assert_eq!((dev.hash.as_str(), dev.package.as_str()), (A_DEV, "a-dev"));
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
}
