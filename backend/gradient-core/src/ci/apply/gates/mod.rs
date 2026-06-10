/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Post-creation parking gates. Each gate moves a freshly-created `Queued`
//! evaluation into `Waiting` for a specific unmet precondition and is a no-op
//! once the eval has left `Queued`. [`run_gates`] threads the eval through all
//! four in order.

mod approval;
mod cache;
mod storage;
mod workers;

use crate::ci::apply::ApprovalInfo;
use gradient_types::*;
use sea_orm::ConnectionTrait;

pub use approval::park_if_pending_approval;
pub use cache::park_if_no_cache;
pub use storage::park_if_storage_full;
pub use workers::park_if_no_workers;

/// Runs the freshly-created evaluation through every parking gate in order:
/// approval → cache → storage → workers. Each gate is a no-op once the eval has
/// left `Queued`, so the first gate that parks short-circuits the rest.
pub(super) async fn run_gates<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    approval: Option<&ApprovalInfo>,
    organization: OrganizationId,
    instance_max_storage_gb: i32,
) -> Result<MEvaluation, sea_orm::DbErr> {
    let eval = park_if_pending_approval(db, eval, approval).await?;
    let eval = park_if_no_cache(db, eval, organization).await?;
    let eval = park_if_storage_full(db, eval, organization, instance_max_storage_gb).await?;
    let eval = park_if_no_workers(db, eval, organization).await?;
    Ok(eval)
}
