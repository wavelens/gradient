/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::sqlx::{AssertSqlSafe, raw_sql};
use sea_orm_migration::sea_orm::{ConnectionTrait, DatabaseExecutor, RuntimeErr};

// Outside a transaction, each statement can use any connection in the pool. The `SET` of `lock_timeout` can land on a different session from `CREATE INDEX`. That setting can stay on a pooled connection and the server can reuse that connection later. All statements run on one connection. A failed statement closes that connection. The connection does not return to the pool with a 5-second `lock_timeout`.
pub async fn run_sql_statements_on_one_connection(
    manager: &SchemaManager<'_>,
    statements: &[&str],
) -> Result<(), DbErr> {
    let session = manager.get_connection();
    let DatabaseExecutor::Connection(db) = session else {
        for statement in statements {
            session.execute_unprepared(statement).await?;
        }
        return Ok(());
    };

    let mut connection = db
        .get_postgres_connection_pool()
        .acquire()
        .await
        .map_err(|error| DbErr::Conn(RuntimeErr::SqlxError(error.into())))?;
    for statement in statements {
        if let Err(error) = raw_sql(AssertSqlSafe(*statement))
            .execute(&mut *connection)
            .await
        {
            connection.close_on_drop();
            return Err(DbErr::Exec(RuntimeErr::SqlxError(error.into())));
        }
    }

    Ok(())
}
