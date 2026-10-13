/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod insert;

use gradient_entity::build::BuildStatus;
use gradient_entity::derivation_dependency::EdgeKind;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::ids::{
    BuildJobId, CacheId, CachedPathId, DerivationBuildId, DerivationId, DerivationOutputId,
    EntryPointId, EvaluationId, ProjectCacheId, ProjectId, TaskId, UserId,
};
use gradient_entity::{
    StorePath, build_job, cache, cached_path, commit, derivation, derivation_build,
    derivation_dependency, derivation_output, entry_point, evaluation, project, project_cache,
    task, user,
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
    evaluation: Option<usize>,
    derivation: Option<usize>,
    cache: Option<usize>,
}

impl Shape {
    pub fn evaluation(mut self, id: EvaluationId) -> Self {
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
        self.evaluation = Some(self.evaluations.len() - 1);
        self
    }

    pub fn evaluation_status(mut self, status: EvaluationStatus) -> Self {
        let at = self.current_evaluation();
        self.evaluations[at].status = status;
        self
    }

    pub fn without_cache_signing(mut self) -> Self {
        let at = self.current_evaluation();
        self.tasks[at].sign_cache = false;
        self
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

    pub fn output(mut self, hash: &str) -> Self {
        let at = self.current_derivation();
        let derivation = self.derivations[at].id;
        let declared = self
            .outputs
            .iter()
            .any(|o| o.derivation == derivation && o.hash == hash);
        if !declared {
            self.outputs.push(derivation_output::Model {
                id: DerivationOutputId::now_v7(),
                derivation,
                name: "out".into(),
                hash: hash.into(),
                package: self.derivations[at].name.clone(),
                created_at: test_date(),
                ..Default::default()
            });
        }
        self
    }

    pub fn job(mut self) -> Self {
        let (evaluation, at) = (self.current_evaluation(), self.current_derivation());
        self.jobs.push(build_job::Model {
            id: BuildJobId::now_v7(),
            evaluation: self.evaluations[evaluation].id,
            derivation: self.derivations[at].id,
            derivation_build: self.shared_builds[at].id,
            created_at: test_date(),
            ..Default::default()
        });
        self
    }

    pub fn entry_point(mut self, attr: &str) -> Self {
        let (evaluation, at) = (self.current_evaluation(), self.current_derivation());
        self.entry_points.push(entry_point::Model {
            id: EntryPointId::now_v7(),
            task: self.tasks[evaluation].id,
            evaluation: self.evaluations[evaluation].id,
            derivation: self.derivations[at].id,
            eval: attr.into(),
            created_at: test_date(),
            ..Default::default()
        });
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
        for output in self
            .outputs
            .iter_mut()
            .filter(|o| o.derivation == derivation)
        {
            output.external_url = Some(format!("https://upstream.invalid/nar/{}.nar", output.hash));
            output.nar_hash = Some("sha256:def".into());
            output.nar_size = Some(5);
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
        let id = self.stored_path(&hash, &package);
        self.outputs[output].is_cached = true;
        self.outputs[output].cached_path = Some(id);
        self
    }

    pub fn into_cache(mut self, id: CacheId) -> Self {
        let owner = self.owner();
        let project = self.projects[self.current_evaluation()].id;
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
        insert::all(self, &db.connection()).await;
        db
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

    fn stored_path(&mut self, hash: &str, package: &str) -> CachedPathId {
        if let Some(existing) = self.cached_paths.iter().find(|p| p.hash == hash) {
            return existing.id;
        }
        let id = CachedPathId::now_v7();
        self.cached_paths.push(cached_path::Model {
            id,
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
        id
    }
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
