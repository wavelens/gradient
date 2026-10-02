/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod approval;
mod cache;
mod storage;
mod workers;

use crate::apply::ApprovalInfo;
use gradient_types::*;
use sea_orm::ConnectionTrait;

pub use approval::park_if_pending_approval;
pub use cache::park_if_no_cache;
pub use storage::park_if_storage_full;
pub use workers::park_if_no_workers;

/// The first parking gate is short-circuiting the rest because each gate is a no-op once the
/// evaluation left `Queued`.
pub(super) async fn run_gates<C: ConnectionTrait>(
    db: &C,
    eval: MEvaluation,
    approval: Option<&ApprovalInfo>,
    project: ProjectId,
    instance_max_storage_gb: i32,
) -> Result<MEvaluation, sea_orm::DbErr> {
    let eval = park_if_pending_approval(db, eval, approval).await?;
    let eval = park_if_no_cache(db, eval, project).await?;
    let eval = park_if_storage_full(db, eval, project, instance_max_storage_gb).await?;
    let eval = park_if_no_workers(db, eval, project).await?;
    Ok(eval)
}
