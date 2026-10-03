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
use tokio::sync::{Notify, Semaphore};
use uuid::Uuid;

use gradient_ci::CiContext;
use gradient_ci::manifest_state::{ManifestStateStore, PendingCredentialsStore};
use gradient_db::metrics::cache_traffic::CacheTraffic;
use gradient_db::{
    CacheDb, DbContext, HeldEvaluations, ProbeRequests, WebDb, WorkerDb,
    scheduling::startable_set::StartableSet,
};
use gradient_git_host::GitHostRegistry;
use gradient_graph::Graph;
use gradient_notify::EmailSender;
use gradient_state::{OidcGroupRoles, PendingProjectMemberships, ScimGroupRoles};
use gradient_storage::{LogStorage, NarStore, StorageCtx};
use gradient_types::{
    BuildProgress, DerivationBuildId, EvaluationId, EvaluationProgress, RuntimeConfig, SecretString,
};
use gradient_util::debounce::Debounce;
use gradient_util::latest::Latest;
use gradient_util::shutdown::Shutdown;

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
    pub upstream_query: Arc<Semaphore>,
    pub upload_admission: Arc<gradient_storage::admission::UploadAdmission>,
    pub git_host: GitHostRegistry,
    pub github_app_install_url: Arc<tokio::sync::OnceCell<String>>,
    pub manifest_state: Arc<ManifestStateStore>,
    pub pending_credentials: Arc<PendingCredentialsStore>,
    pub shutdown: Shutdown,
    pub last_used_stamps: Debounce<Uuid>,
    pub cache_traffic: Arc<CacheTraffic>,
    pub jwt_secret: SecretString,
    pub started_at: DateTime<Utc>,
    pub pending_project_memberships: Arc<PendingProjectMemberships>,
    pub oidc_group_roles: Arc<OidcGroupRoles>,
    pub scim_group_roles: Arc<ScimGroupRoles>,
    pub events: gradient_types::EventBus,
    pub build_progress: Arc<Latest<DerivationBuildId, BuildProgress>>,
    pub eval_progress: Arc<Latest<EvaluationId, EvaluationProgress>>,
    pub delivery_wake: Arc<Notify>,
    pub eval_assign_wake: Arc<Notify>,
    pub graph: Arc<Graph>,
    pub probe_requests: ProbeRequests,
    pub held_evaluations: HeldEvaluations,
    pub startable_set: StartableSet,
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
            delivery_wake: self.delivery_wake.clone(),
            probe_requests: self.probe_requests.clone(),
            held_evaluations: self.held_evaluations.clone(),
            startable_set: self.startable_set.clone(),
        }
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
        let event = event.into();
        let name = event.name();
        if let Err(e) =
            gradient_db::deliveries::events::record(&self.worker_db, &self.events, event).await
        {
            tracing::error!(error = %e, event = %name, "failed to record an event");
        }
        self.delivery_wake.notify_one();
    }

    pub async fn record_evaluation_created(&self, eval: &gradient_types::MEvaluation) {
        self.eval_assign_wake.notify_one();
        if let Some(event) = gradient_db::deliveries::events::evaluation_created(eval) {
            self.record(event).await;
        }
    }
}
