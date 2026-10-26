/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Nothing below this facade may name `AppState`. Lower layers are taking the projected
//! [`StorageCtx`], [`DbContext`] and [`CiContext`] slices.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use uuid::Uuid;

use gradient_ci::CiContext;
use gradient_ci::manifest_state::{ManifestStateStore, PendingCredentialsStore};
use gradient_db::metrics::cache_traffic::CacheTraffic;
use gradient_db::task_board::BuildStatusCountsCache;
use gradient_db::{CacheDb, DbContext, DbMemory, Wakeups, WebDb, WorkerDb};
use gradient_entity::ids::{CacheId, ProjectId};
use gradient_entity::{MappingError, StorePathMapper};
use gradient_git_host::GitHostRegistry;
use gradient_graph::Graph;
use gradient_notify::EmailSender;
use gradient_state::PendingProjectMemberships;
use gradient_storage::{LogStorage, NarStore, StorageCtx};
use gradient_types::ids::ClusterAttemptId;
use gradient_types::{
    BuildProgress, DerivationBuildId, EvaluationId, EvaluationProgress, RuntimeConfig, SecretString,
};
use gradient_util::debounce::Debounce;
use gradient_util::latest::Latest;
use gradient_util::member_sums::MemberSums;
use gradient_util::shutdown::Shutdown;

use crate::ServerExtensions;

#[derive(Debug)]
pub struct InstanceMemory {
    pub db: DbMemory,
    pub last_used_stamps: Debounce<Uuid>,
    pub cache_traffic: Arc<CacheTraffic>,
    pub build_progress: Arc<Latest<DerivationBuildId, BuildProgress>>,
    pub eval_progress: Arc<Latest<EvaluationId, EvaluationProgress>>,
    pub eval_member_thunks: Arc<EvalMemberThunks>,
    pub build_status_counts: Arc<BuildStatusCountsCache>,
    pub github_app_install_url: Arc<tokio::sync::OnceCell<String>>,
    pub manifest_state: ManifestStateStore,
    pub pending_credentials: PendingCredentialsStore,
    pub pending_project_memberships: PendingProjectMemberships,
    pub upstream_query: Arc<Semaphore>,
    pub nar_downloads: Arc<Semaphore>,
    pub upload_admission: Arc<gradient_storage::admission::UploadAdmission>,
}

#[derive(Debug)]
pub struct AppState {
    pub worker_db: WorkerDb,
    pub web_db: WebDb,
    /// A dedicated pool is keeping a large evaluation's prefetch storm from exhausting
    /// [`Self::worker_db`] and stalling the scheduler.
    pub cache_db: CacheDb,
    pub config: Arc<RuntimeConfig>,
    pub log_storage: Arc<dyn LogStorage>,
    pub email: Arc<dyn EmailSender>,
    pub nar_storage: NarStore,
    pub http: reqwest::Client,
    pub git_host: GitHostRegistry,
    pub shutdown: Shutdown,
    pub jwt_secret: SecretString,
    pub started_at: DateTime<Utc>,
    pub events: gradient_types::EventBus,
    pub graph: Arc<Graph>,
    pub memory: InstanceMemory,
    pub wake: Wakeups,
    pub extensions: ServerExtensions,
}

pub type ServerState = AppState;

pub const LAST_USED_STAMP_INTERVAL: Duration = Duration::from_secs(60);

pub fn last_used_stamps() -> Debounce<Uuid> {
    Debounce::new(LAST_USED_STAMP_INTERVAL)
}

pub const BUILD_PROGRESS_TTL: Duration = Duration::from_secs(15);

pub fn build_progress() -> Arc<Latest<DerivationBuildId, BuildProgress>> {
    Arc::new(Latest::new(BUILD_PROGRESS_TTL))
}

pub const EVAL_PROGRESS_TTL: Duration = Duration::from_secs(60);

pub fn eval_progress() -> Arc<Latest<EvaluationId, EvaluationProgress>> {
    Arc::new(Latest::new(EVAL_PROGRESS_TTL))
}

pub type EvalMemberThunks = MemberSums<EvaluationId, Option<ClusterAttemptId>>;

pub fn eval_member_thunks() -> Arc<EvalMemberThunks> {
    Arc::new(MemberSums::new(EVAL_PROGRESS_TTL))
}

pub fn build_status_counts() -> Arc<BuildStatusCountsCache> {
    Arc::new(Latest::new(Duration::from_secs(600)))
}

impl AppState {
    pub fn storage(&self) -> StorageCtx {
        StorageCtx {
            nar_storage: self.nar_storage.clone(),
            log_storage: self.log_storage.clone(),
        }
    }

    pub fn db(&self) -> DbContext {
        DbContext {
            worker_db: self.worker_db.clone(),
            web_db: self.web_db.clone(),
            config: self.config.clone(),
            storage: self.storage(),
            shutdown: self.shutdown.clone(),
            events: self.events.clone(),
            wake: self.wake.clone(),
            memory: self.memory.db.clone(),
            store_path_mapping: Arc::clone(&self.extensions.store_path_mapping),
        }
    }

    pub fn cache_mapper(&self, cache: CacheId) -> StorePathMapper {
        StorePathMapper::for_cache(Arc::clone(&self.extensions.store_path_mapping), cache)
    }

    pub async fn project_mapper(
        &self,
        project: ProjectId,
    ) -> Result<StorePathMapper, MappingError> {
        StorePathMapper::for_project(Arc::clone(&self.extensions.store_path_mapping), project).await
    }

    pub fn ci(&self) -> CiContext {
        CiContext {
            db: self.db(),
            http: self.http.clone(),
            git_host: self.git_host.clone(),
            email: self.email.clone(),
        }
    }

    pub async fn record(&self, event: impl Into<gradient_types::Event>) {
        self.db().record(event).await;
    }

    pub async fn record_evaluation_created(&self, eval: &gradient_types::MEvaluation) {
        self.db().record_evaluation_created(eval).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gradient_entity::ids::ProjectId;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};

    #[tokio::test]
    async fn the_server_mapping_reaches_the_database_context() {
        let state = gradient_test_support::state::TestState::new()
            .store_path_mapping(Arc::new(RotatedHashMapping))
            .build();
        let mapper = state.db().project_mapper(ProjectId::nil()).await.unwrap();
        assert_eq!(
            mapper.outbound_text(EXAMPLE_STORED_HASH).unwrap(),
            EXAMPLE_ORIGINAL_HASH
        );
    }
}
