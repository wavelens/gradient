/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE ONLY public.evaluation
    DROP CONSTRAINT "fk-evaluation-next",
    DROP CONSTRAINT "fk-evaluation-previous",
    ADD CONSTRAINT "fk-evaluation-next" FOREIGN KEY (next) REFERENCES public.evaluation(id) ON DELETE SET NULL NOT VALID,
    ADD CONSTRAINT "fk-evaluation-previous" FOREIGN KEY (previous) REFERENCES public.evaluation(id) ON DELETE SET NULL NOT VALID;
"#;

const DOWN: &str = r#"
ALTER TABLE ONLY public.evaluation
    DROP CONSTRAINT "fk-evaluation-next",
    DROP CONSTRAINT "fk-evaluation-previous",
    ADD CONSTRAINT "fk-evaluation-next" FOREIGN KEY (next) REFERENCES public.evaluation(id) ON DELETE CASCADE NOT VALID,
    ADD CONSTRAINT "fk-evaluation-previous" FOREIGN KEY (previous) REFERENCES public.evaluation(id) ON DELETE CASCADE NOT VALID;
"#;

const VALIDATE: [&str; 2] = [
    "ALTER TABLE public.evaluation VALIDATE CONSTRAINT \"fk-evaluation-next\";",
    "ALTER TABLE public.evaluation VALIDATE CONSTRAINT \"fk-evaluation-previous\";",
];

async fn replace_evaluation_previous_next_foreign_keys_then_validate(
    manager: &SchemaManager<'_>,
    sql: &str,
) -> Result<(), DbErr> {
    let transaction = manager.begin().await?;
    transaction.get_connection().execute_unprepared(sql).await?;
    transaction.commit().await?;
    for statement in VALIDATE {
        manager
            .get_connection()
            .execute_unprepared(statement)
            .await?;
    }
    Ok(())
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_evaluation_previous_next_foreign_keys_then_validate(manager, UP).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        replace_evaluation_previous_next_foreign_keys_then_validate(manager, DOWN).await
    }
}
