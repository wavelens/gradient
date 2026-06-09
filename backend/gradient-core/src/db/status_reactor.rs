/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Inversion point for the former `db -> ci` edge: `db` defines and *awaits*
//! this trait when a build/evaluation reaches a terminal status; the `ci` layer
//! implements it. The call stays an in-process awaited trait call, so ordering
//! and error handling match the prior inline dispatch exactly.

use std::sync::Arc;

use async_trait::async_trait;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;

use crate::types::{MBuild, MEvaluation, ServerState};

#[async_trait]
pub trait StatusReactor: Send + Sync + std::fmt::Debug {
    async fn on_build_terminal(&self, state: &Arc<ServerState>, build: MBuild, status: BuildStatus);
    async fn on_eval_terminal(
        &self,
        state: &Arc<ServerState>,
        evaluation: MEvaluation,
        status: EvaluationStatus,
    );
}

/// No-op reactor for tests and worker-side flows that never react.
#[derive(Debug)]
pub struct NoReactor;

#[async_trait]
impl StatusReactor for NoReactor {
    async fn on_build_terminal(&self, _: &Arc<ServerState>, _: MBuild, _: BuildStatus) {}
    async fn on_eval_terminal(&self, _: &Arc<ServerState>, _: MEvaluation, _: EvaluationStatus) {}
}
