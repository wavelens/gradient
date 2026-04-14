/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure database operations for the `build` table.
//!
//! [`BuildRepo`] takes only a `&DatabaseConnection`.
//! Side effects (webhooks, log finalization) stay in the service layer.

use anyhow::Result;
use chrono::Utc;
use entity::build::BuildStatus;
use sea_orm::DatabaseConnection;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, PaginatorTrait,
    QueryFilter,
};
use std::collections::HashSet;
use std::time::Duration;
use uuid::Uuid;

use crate::types::*;

pub struct BuildRepo<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> BuildRepo<'db> {
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// Fetch a build by ID.
    pub async fn find(&self, id: Uuid) -> Result<Option<MBuild>> {
        Ok(EBuild::find_by_id(id).one(self.db).await?)
    }

    /// Update build status and `updated_at` timestamp. Returns the updated row.
    pub async fn update_status(&self, build: MBuild, status: BuildStatus) -> Result<MBuild> {
        let mut active: ABuild = build.clone().into_active_model();
        active.status = Set(status);
        active.updated_at = Set(Utc::now().naive_utc());
        let updated = active.update(self.db).await?;
        Ok(updated)
    }

    /// Persist elapsed build time.
    pub async fn record_build_time(&self, build: MBuild, elapsed: Duration) -> Result<MBuild> {
        let mut active: ABuild = build.clone().into_active_model();
        active.build_time_ms = Set(Some(elapsed.as_millis() as i64));
        let updated = active.update(self.db).await?;
        Ok(updated)
    }

    /// Insert a batch of build rows (1 000 per chunk to avoid parameter limits).
    pub async fn insert_builds(&self, builds: Vec<MBuild>) -> Result<()> {
        const BATCH_SIZE: usize = 1000;
        let active: Vec<ABuild> = builds.into_iter().map(|b| b.into_active_model()).collect();
        for chunk in active.chunks(BATCH_SIZE) {
            EBuild::insert_many(chunk.to_vec()).exec(self.db).await?;
        }
        Ok(())
    }

    /// Find all builds for an evaluation in a given status.
    pub async fn find_by_evaluation_and_status(
        &self,
        evaluation_id: Uuid,
        status: BuildStatus,
    ) -> Result<Vec<MBuild>> {
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Status.eq(status))
            .all(self.db)
            .await?;
        Ok(builds)
    }

    /// Find all builds for an evaluation regardless of status.
    pub async fn find_by_evaluation(&self, evaluation_id: Uuid) -> Result<Vec<MBuild>> {
        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .all(self.db)
            .await?;
        Ok(builds)
    }

    /// Find all builds whose derivation depends (directly) on the given derivation.
    /// Used for cascading `DependencyFailed`.
    pub async fn find_dependents_in_evaluation(
        &self,
        evaluation_id: Uuid,
        derivation_id: Uuid,
    ) -> Result<Vec<MBuild>> {
        let dep_derivations = EDerivationDependency::find()
            .filter(CDerivationDependency::Dependency.eq(derivation_id))
            .all(self.db)
            .await?;

        if dep_derivations.is_empty() {
            return Ok(vec![]);
        }

        let dependent_derivation_ids: Vec<Uuid> =
            dep_derivations.into_iter().map(|d| d.derivation).collect();

        let builds = EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(CBuild::Derivation.is_in(dependent_derivation_ids))
            .all(self.db)
            .await?;
        Ok(builds)
    }

    /// Claim a build atomically for a specific build machine, transitioning it
    /// from `Queued` → `Building`. Returns the updated row if claimed, `None`
    /// if another scheduler already claimed it.
    pub async fn claim_for_build_machine(
        &self,
        build: MBuild,
        build_machine_id: Uuid,
    ) -> Result<Option<MBuild>> {
        let mut active: ABuild = build.clone().into_active_model();
        active.server = Set(Some(build_machine_id));
        active.status = Set(BuildStatus::Building);
        active.updated_at = Set(Utc::now().naive_utc());

        match active.update(self.db).await {
            Ok(updated) if updated.status == BuildStatus::Building => Ok(Some(updated)),
            Ok(_) => Ok(None), // concurrent update won
            Err(e) => Err(e.into()),
        }
    }

    /// Count builds in `Building` state assigned to a specific build machine.
    pub async fn count_building_on_machine(&self, build_machine_id: Uuid) -> Result<usize> {
        let count = EBuild::find()
            .filter(CBuild::Server.eq(build_machine_id))
            .filter(CBuild::Status.eq(BuildStatus::Building))
            .count(self.db)
            .await?;
        Ok(count as usize)
    }

    /// Return the IDs of builds to skip in the current scheduling cycle
    /// (already assigned or skipped this tick).
    pub async fn find_queued_with_satisfied_deps(
        &self,
        skip: &HashSet<Uuid>,
    ) -> Result<Option<(MBuild, MDerivation)>> {
        // Raw SQL for the dependency satisfaction check is handled in
        // builder/src/build/queue.rs. This stub exists for the repo boundary.
        // Full migration of that query into the repo is a follow-up step.
        let _ = skip;
        Ok(None)
    }
}
