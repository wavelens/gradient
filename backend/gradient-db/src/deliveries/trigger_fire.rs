/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use gradient_types::TaskTriggerId;
use sea_orm::{ConnectionTrait, DbErr};
use serde::{Deserialize, Serialize};

use super::pending::{PendingDeliveryKind, enqueue_many};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerFire {
    pub trigger: TaskTriggerId,
    pub last_fired_at: Option<NaiveDateTime>,
}

impl TriggerFire {
    pub fn key(&self) -> String {
        match self.last_fired_at {
            Some(at) => format!("{}:{at}", self.trigger),
            None => format!("{}:never", self.trigger),
        }
    }
}

pub async fn enqueue_trigger_fires<C: ConnectionTrait>(
    db: &C,
    fires: &[TriggerFire],
) -> Result<(), DbErr> {
    let rows = fires
        .iter()
        .map(|fire| {
            let payload = serde_json::to_value(fire)
                .map_err(|e| DbErr::Custom(format!("encode a trigger fire: {e}")))?;
            Ok((fire.key(), payload))
        })
        .collect::<Result<Vec<_>, DbErr>>()?;

    enqueue_many(db, PendingDeliveryKind::TriggerFire, rows).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{all_statements, exec_ok, mock_db};

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn the_key_names_the_trigger_and_the_firing_it_answers() {
        let trigger = TaskTriggerId::now_v7();

        let first = TriggerFire {
            trigger,
            last_fired_at: None,
        };
        let later = TriggerFire {
            trigger,
            last_fired_at: Some(at("2026-05-06 10:00:00")),
        };

        assert_eq!(first.key(), format!("{trigger}:never"));
        assert_eq!(later.key(), format!("{trigger}:2026-05-06 10:00:00"));
    }

    #[test]
    fn the_payload_decodes_to_what_was_encoded() {
        let fire = TriggerFire {
            trigger: TaskTriggerId::now_v7(),
            last_fired_at: Some(at("2026-05-06 10:00:00")),
        };

        let payload = serde_json::to_value(fire).unwrap();

        assert_eq!(
            serde_json::from_value::<TriggerFire>(payload).unwrap(),
            fire
        );
    }

    #[tokio::test]
    async fn a_batch_of_firings_is_one_statement_keyed_per_firing() {
        let fires = [
            TriggerFire {
                trigger: TaskTriggerId::now_v7(),
                last_fired_at: None,
            },
            TriggerFire {
                trigger: TaskTriggerId::now_v7(),
                last_fired_at: Some(at("2026-05-06 10:00:00")),
            },
        ];
        let db = mock_db().append_exec_results([exec_ok()]).into_connection();

        enqueue_trigger_fires(&db, &fires).await.unwrap();

        let log = all_statements(db.into_transaction_log());
        assert_eq!(log.len(), 1);
        let mut keys: Vec<String> = fires.iter().map(TriggerFire::key).collect();
        keys.sort();
        let expected: sea_orm::Value = keys.into();
        assert_eq!(log[0].values.clone().unwrap().0[2], expected);
    }
}
