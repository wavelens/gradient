/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation_message::MessageLevel;
use gradient_wire::types::{ATTR_EVAL_SOURCE_PREFIX, attr_of_eval_source};
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder};

use gradient_types::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAttribute {
    pub attr: String,
    pub message: String,
}

pub async fn failed_attributes<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<FailedAttribute>, DbErr> {
    let messages = EEvaluationMessage::find()
        .filter(CEvaluationMessage::Evaluation.eq(evaluation))
        .filter(CEvaluationMessage::Level.eq(MessageLevel::Error))
        .filter(CEvaluationMessage::Source.starts_with(ATTR_EVAL_SOURCE_PREFIX))
        .order_by_asc(CEvaluationMessage::Source)
        .order_by_asc(CEvaluationMessage::CreatedAt)
        .all(db)
        .await?;

    Ok(dedup_by_attribute(messages))
}

fn dedup_by_attribute(messages: Vec<MEvaluationMessage>) -> Vec<FailedAttribute> {
    let mut failed: Vec<FailedAttribute> = Vec::new();
    for m in messages {
        let Some(attr) = m.source.as_deref().and_then(attr_of_eval_source) else {
            continue;
        };
        if failed.last().is_some_and(|f| f.attr == attr) {
            continue;
        }
        failed.push(FailedAttribute {
            attr: attr.to_owned(),
            message: m.message,
        });
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(source: &str, text: &str) -> MEvaluationMessage {
        MEvaluationMessage {
            source: Some(source.to_owned()),
            message: text.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn a_retried_evaluation_reports_each_attribute_once() {
        let failed = dedup_by_attribute(vec![
            message("nix-eval:nixosConfigurations.a", "first"),
            message("nix-eval:nixosConfigurations.a", "retry"),
            message("nix-eval:nixosConfigurations.b", "other"),
        ]);

        assert_eq!(
            failed,
            vec![
                FailedAttribute {
                    attr: "nixosConfigurations.a".into(),
                    message: "first".into(),
                },
                FailedAttribute {
                    attr: "nixosConfigurations.b".into(),
                    message: "other".into(),
                },
            ]
        );
    }
}
