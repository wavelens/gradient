/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure database operations for derivation-related tables.

use anyhow::Result;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder,
    QuerySelect,
};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::types::*;

pub struct DerivationRepo<'db> {
    db: &'db DatabaseConnection,
}

impl<'db> DerivationRepo<'db> {
    pub fn new(db: &'db DatabaseConnection) -> Self {
        Self { db }
    }

    /// Insert derivation rows in 1 000-row batches.
    pub async fn insert_derivations(&self, derivations: Vec<MDerivation>) -> Result<()> {
        const BATCH_SIZE: usize = 1000;
        let active: Vec<ADerivation> =
            derivations.into_iter().map(|d| d.into_active_model()).collect();
        for chunk in active.chunks(BATCH_SIZE) {
            EDerivation::insert_many(chunk.to_vec()).exec(self.db).await?;
        }
        Ok(())
    }

    /// Insert derivation output rows in 1 000-row batches.
    pub async fn insert_outputs(&self, outputs: Vec<ADerivationOutput>) -> Result<()> {
        const BATCH_SIZE: usize = 1000;
        for chunk in outputs.chunks(BATCH_SIZE) {
            EDerivationOutput::insert_many(chunk.to_vec()).exec(self.db).await?;
        }
        Ok(())
    }

    /// Insert derivation dependency edges in 1 000-row batches.
    pub async fn insert_dependencies(&self, deps: Vec<MDerivationDependency>) -> Result<()> {
        const BATCH_SIZE: usize = 1000;
        let active: Vec<ADerivationDependency> =
            deps.into_iter().map(|d| d.into_active_model()).collect();
        for chunk in active.chunks(BATCH_SIZE) {
            EDerivationDependency::insert_many(chunk.to_vec()).exec(self.db).await?;
        }
        Ok(())
    }

    /// Return up to `limit` uncached derivation outputs, oldest first.
    pub async fn find_uncached_outputs(&self, limit: usize) -> Result<Vec<MDerivationOutput>> {
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::IsCached.eq(false))
            .order_by_asc(CDerivationOutput::CreatedAt)
            .limit(limit as u64)
            .all(self.db)
            .await?;
        Ok(outputs)
    }

    /// Mark a derivation output as cached.
    pub async fn mark_output_cached(&self, output: MDerivationOutput) -> Result<MDerivationOutput> {
        let mut active: ADerivationOutput = output.into_active_model();
        active.is_cached = sea_orm::ActiveValue::Set(true);
        let updated = active.update(self.db).await?;
        Ok(updated)
    }

    /// Find all outputs for a derivation.
    pub async fn find_outputs_for_derivation(
        &self,
        derivation_id: Uuid,
    ) -> Result<Vec<MDerivationOutput>> {
        let outputs = EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.eq(derivation_id))
            .all(self.db)
            .await?;
        Ok(outputs)
    }

    /// Find all outputs by store path.
    pub async fn find_output_by_path(
        &self,
        store_path: &str,
    ) -> Result<Option<MDerivationOutput>> {
        let output = EDerivationOutput::find()
            .filter(CDerivationOutput::Output.eq(store_path))
            .one(self.db)
            .await?;
        Ok(output)
    }
}
