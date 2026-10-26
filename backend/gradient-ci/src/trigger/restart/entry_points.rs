/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::trigger::TriggerError;
use chrono::NaiveDateTime;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, ConnectionTrait, IntoActiveModel};

pub(super) async fn copy_entry_points<C: ConnectionTrait>(
    db: &C,
    prev_entry_points: &[MEntryPoint],
    new_eval_id: EvaluationId,
    now: NaiveDateTime,
) -> Result<(), TriggerError> {
    for prev_ep in prev_entry_points {
        let aep = MEntryPoint {
            id: EntryPointId::now_v7(),
            task: prev_ep.task,
            evaluation: new_eval_id,
            derivation: prev_ep.derivation,
            eval: prev_ep.eval.clone(),
            licenses: prev_ep.licenses.clone(),
            runner: prev_ep.runner.clone(),
            created_at: now,
            ..Default::default()
        }
        .into_active_model();

        aep.insert(db).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::entry_point::{License, Licenses};
    use gradient_fixtures::mock::{mock_db, raw_statements};

    #[tokio::test]
    async fn a_restart_keeps_the_licenses() {
        let previous = MEntryPoint {
            licenses: Licenses(vec![License::Name {
                name: "unfree".into(),
            }]),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![previous.clone()]])
            .into_connection();

        copy_entry_points(
            &db,
            &[previous],
            EvaluationId::now_v7(),
            gradient_types::now(),
        )
        .await
        .unwrap();

        let log = raw_statements(db.into_transaction_log());
        assert!(
            log.iter()
                .any(|s| s.sql.contains("INSERT INTO \"entry_point\"")
                    && format!("{:?}", s.values).contains("unfree")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn a_restart_keeps_the_runner_spec() {
        let previous = MEntryPoint {
            runner: Some(serde_json::json!({ "program": "/nix/store/aaaa-train/bin/train" })),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![previous.clone()]])
            .into_connection();

        copy_entry_points(
            &db,
            &[previous],
            EvaluationId::now_v7(),
            gradient_types::now(),
        )
        .await
        .unwrap();

        let log = raw_statements(db.into_transaction_log());
        assert!(
            log.iter()
                .any(|s| s.sql.contains("INSERT INTO \"entry_point\"")
                    && format!("{:?}", s.values).contains("aaaa-train")),
            "{log:?}"
        );
    }
}
