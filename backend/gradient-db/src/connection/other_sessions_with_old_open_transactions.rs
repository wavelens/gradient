/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use sea_orm::{ConnectionTrait, DbErr, Value};

crate::sql! {
    SELECT_SESSIONS_WITH_TRANSACTION_STARTED_MORE_THAN_MILLISECONDS_AGO_EXCLUDING_OWN_BACKEND = "
        SELECT pid,
               application_name,
               (extract(epoch FROM now() - xact_start) * 1000)::int8
                   AS current_transaction_age_milliseconds,
               state,
               wait_event_type,
               left(query, 120) AS query
        FROM pg_stat_activity
        WHERE xact_start < now() - $1 * interval '1 millisecond'
          AND pid <> pg_backend_pid()
          AND datname = current_database()
        ORDER BY xact_start",
        params = [Int(10_000)];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostgresSessionWithLongTransaction {
    pub pid: i32,
    pub application_name: Option<String>,
    pub current_transaction_age_milliseconds: i64,
    pub state: Option<String>,
    pub wait_event_type: Option<String>,
    pub query: Option<String>,
}

pub async fn find_sessions_with_transaction_older_than<C: ConnectionTrait>(
    db: &C,
    age: Duration,
) -> Result<Vec<PostgresSessionWithLongTransaction>, DbErr> {
    let age_milliseconds = i64::try_from(age.as_millis()).unwrap_or(i64::MAX);
    let rows = db
        .query_all_raw(
            SELECT_SESSIONS_WITH_TRANSACTION_STARTED_MORE_THAN_MILLISECONDS_AGO_EXCLUDING_OWN_BACKEND
                .bind([Value::from(age_milliseconds)]),
        )
        .await?;

    rows.iter()
        .map(|row| {
            Ok(PostgresSessionWithLongTransaction {
                pid: row.try_get("", "pid")?,
                application_name: row.try_get("", "application_name")?,
                current_transaction_age_milliseconds: row
                    .try_get("", "current_transaction_age_milliseconds")?,
                state: row.try_get("", "state")?,
                wait_event_type: row.try_get("", "wait_event_type")?,
                query: row.try_get("", "query")?,
            })
        })
        .collect()
}
