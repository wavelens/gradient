/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::WebResult;
use crate::helpers::ok_json;
use crate::metrics_scope::MetricsScope;
use axum::extract::State;
use axum::{Extension, Json};
use gradient_core::ServerState;
use gradient_db::dashboard::{ActivityDay, activity};
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct Activity {
    pub days: Vec<ActivityDay>,
}

pub async fn load_activity<C: ConnectionTrait>(
    db: &C,
    scope: &MetricsScope,
) -> Result<Vec<ActivityDay>, DbErr> {
    match scope.project_in_list().as_deref() {
        Some("") => Ok(Vec::new()),
        filter => activity(db, filter).await,
    }
}

pub async fn get_activity(
    State(state): State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
) -> WebResult<Json<BaseResponse<Activity>>> {
    let scope = MetricsScope::resolve(&state.web_db, &Some(user)).await?;
    Ok(ok_json(Activity {
        days: load_activity(&state.web_db, &scope).await?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase};

    #[tokio::test]
    async fn an_empty_scope_skips_the_query() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let days = load_activity(&db, &MetricsScope::Projects(vec![]))
            .await
            .unwrap();
        assert!(days.is_empty());
        assert!(db.into_transaction_log().is_empty());
    }
}
