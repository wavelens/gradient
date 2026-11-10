/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::insert_evaluation_message;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr};
use tracing::warn;

/// Add a variant for every new warning of this process. Use `insert_worker_message` for text
/// from a worker.
#[derive(Debug, thiserror::Error)]
pub enum EvaluationWarning<'a> {
    #[error(
        "upstream cache {url} did not answer; its outputs wait for it instead of being built. Deactivate the upstream to build them"
    )]
    UpstreamUnanswered { url: &'a str },
    #[error(
        "Aborted: no connected worker provides {}. Enable waiting for workers on the task to wait instead.",
        systems(.unmet)
    )]
    NoWorker { unmet: &'a [UnmetRequirement] },
    #[error(
        "artefact {path} ({file_type} {subtype}) is declared in hydra-build-products but missing from the build output"
    )]
    ArtefactMissing {
        path: &'a str,
        file_type: &'a str,
        subtype: &'a str,
    },
    #[error("substitution of {store_path} exhausted after {misses} misses; it will be built")]
    SubstitutionExhausted { store_path: &'a str, misses: i64 },
}

fn systems(unmet: &[UnmetRequirement]) -> String {
    let systems: Vec<String> = unmet.iter().map(ToString::to_string).collect();

    systems.join("; ")
}

impl EvaluationWarning<'_> {
    fn source(&self) -> &'static str {
        match self {
            Self::UpstreamUnanswered { .. } => "upstream-probe",
            Self::NoWorker { .. } | Self::SubstitutionExhausted { .. } => "scheduler",
            Self::ArtefactMissing { .. } => "builder",
        }
    }

    pub async fn insert(
        &self,
        db: &impl ConnectionTrait,
        evaluation: EvaluationId,
    ) -> Result<(), DbErr> {
        let source = Some(self.source().to_owned());
        insert_evaluation_message(
            db,
            evaluation,
            MessageLevel::Warning,
            self.to_string(),
            source,
        )
        .await
    }

    pub async fn report(&self, db: &impl ConnectionTrait, evaluation: EvaluationId) {
        if let Err(e) = self.insert(db, evaluation).await {
            warn!(error = %e, %evaluation, warning = %self, "failed to store the evaluation warning");
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationFailure<'a> {
    #[error(
        "evaluation was dispatched {assignments} times and each worker disconnected before reporting a result; giving up"
    )]
    OrphanedOnEveryDispatch { assignments: u64 },
    #[error("{0}")]
    ReportedByWorker(&'a str),
    #[error("{0}")]
    GraphInsert(&'a str),
}

impl EvaluationFailure<'_> {
    pub(super) fn source(&self) -> &'static str {
        match self {
            Self::OrphanedOnEveryDispatch { .. } => "scheduler",
            Self::ReportedByWorker(_) => "worker",
            Self::GraphInsert(_) => "db-insert",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::shape::shape;

    fn unmet(architecture: &str, features: &[&str], build_count: u32) -> UnmetRequirement {
        UnmetRequirement {
            architecture: architecture.into(),
            required_features: features.iter().map(|f| f.to_string()).collect(),
            build_count,
        }
    }

    #[test]
    fn no_worker_names_every_missing_architecture_and_feature() {
        let unmet = [
            unmet("aarch64-linux", &[], 142),
            unmet("x86_64-linux", &["kvm", "nixos-test"], 3),
        ];
        let warning = EvaluationWarning::NoWorker { unmet: &unmet }.to_string();

        assert!(warning.contains("aarch64-linux (142 builds)"));
        assert!(warning.contains("x86_64-linux with features kvm, nixos-test (3 builds)"));
    }

    #[tokio::test]
    async fn repeated_text_is_stored_once_per_level() {
        let evaluation = EvaluationId::now_v7();
        let db = shape().evaluation(evaluation).into_database().await;
        let connection = db.connection();
        let warning = EvaluationWarning::UpstreamUnanswered {
            url: "https://cache.example",
        };
        let text = warning.to_string();

        for _ in 0..2 {
            warning
                .insert(&connection, evaluation)
                .await
                .expect("the warning is written");
        }
        insert_evaluation_message(
            &connection,
            evaluation,
            MessageLevel::Error,
            text.clone(),
            Some("worker".to_owned()),
        )
        .await
        .expect("the error is written");

        let mut rows = db.rows::<EEvaluationMessage>().await;
        rows.sort_by_key(|row| row.id);
        let levels: Vec<MessageLevel> = rows.into_iter().map(|row| row.level).collect();
        assert_eq!(levels, [MessageLevel::Warning, MessageLevel::Error]);
        assert_eq!(db.error_messages(evaluation).await, [text]);
        db.finish().await;
    }
}
