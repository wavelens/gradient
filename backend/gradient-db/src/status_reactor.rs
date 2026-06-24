/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Inversion point for the former `db -> ci` edge: `db` defines and *awaits*
//! this trait when a build/evaluation changes status; the `ci` layer implements
//! it. The call stays an in-process awaited trait call, so ordering and error
//! handling match the prior inline dispatch exactly.

use async_trait::async_trait;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;

use super::DbContext;
use gradient_types::{MBuildJob, MEvaluation};

#[async_trait]
pub trait StatusReactor: Send + Sync + std::fmt::Debug {
    /// Called once per referencing eval's `build_job` whenever its anchor changes
    /// to a status the CI side reports (`Queued`, `Building`, or terminal). The
    /// `build_job` carries the eval + derivation needed to post per-target CI
    /// status.
    async fn on_build_status_changed(
        &self,
        ctx: &DbContext,
        build_job: MBuildJob,
        status: BuildStatus,
    );
    async fn on_eval_terminal(
        &self,
        ctx: &DbContext,
        evaluation: MEvaluation,
        status: EvaluationStatus,
    );
}

/// No-op reactor for tests and worker-side flows that never react.
#[derive(Debug)]
pub struct NoReactor;

#[async_trait]
impl StatusReactor for NoReactor {
    async fn on_build_status_changed(&self, _: &DbContext, _: MBuildJob, _: BuildStatus) {}
    async fn on_eval_terminal(&self, _: &DbContext, _: MEvaluation, _: EvaluationStatus) {}
}
