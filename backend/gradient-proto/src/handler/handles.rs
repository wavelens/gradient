/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_core::{EvalMemberThunks, ServerState};
use gradient_db::metrics::cache_traffic::CacheTraffic;
use gradient_db::{CacheDb, DbContext};
use gradient_graph::Graph;
use gradient_storage::admission::UploadAdmission;
use gradient_types::{BuildProgress, DerivationBuildId, EvaluationId, EvaluationProgress};
use gradient_util::latest::Latest;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct SessionHandles {
    pub db: DbContext,
    pub cache_db: CacheDb,
    pub graph: Arc<Graph>,
    pub nar_downloads: Arc<Semaphore>,
    pub upstream_query: Arc<Semaphore>,
    pub upload_admission: Arc<UploadAdmission>,
    pub cache_traffic: Arc<CacheTraffic>,
    pub build_progress: Arc<Latest<DerivationBuildId, BuildProgress>>,
    pub eval_progress: Arc<Latest<EvaluationId, EvaluationProgress>>,
    pub eval_member_thunks: Arc<EvalMemberThunks>,
}

impl SessionHandles {
    pub fn from_state(state: &ServerState) -> Self {
        Self {
            db: state.db(),
            cache_db: state.cache_db.clone(),
            graph: Arc::clone(&state.graph),
            nar_downloads: Arc::clone(&state.memory.nar_downloads),
            upstream_query: Arc::clone(&state.memory.upstream_query),
            upload_admission: Arc::clone(&state.memory.upload_admission),
            cache_traffic: Arc::clone(&state.memory.cache_traffic),
            build_progress: Arc::clone(&state.memory.build_progress),
            eval_progress: Arc::clone(&state.memory.eval_progress),
            eval_member_thunks: Arc::clone(&state.memory.eval_member_thunks),
        }
    }
}
