/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::logging::{insert_evaluation_message, insert_evaluation_message_once};
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

    fn is_raised_on_every_pass(&self) -> bool {
        matches!(self, Self::UpstreamUnanswered { .. })
    }

    pub async fn insert(
        &self,
        db: &impl ConnectionTrait,
        evaluation: EvaluationId,
    ) -> Result<(), DbErr> {
        let message = self.to_string();
        let source = Some(self.source().to_owned());
        if self.is_raised_on_every_pass() {
            let level = MessageLevel::Warning;

            return insert_evaluation_message_once(db, evaluation, level, message, source).await;
        }

        insert_evaluation_message(db, evaluation, MessageLevel::Warning, message, source).await
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
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

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
    async fn an_unanswered_upstream_is_stored_only_for_its_first_pass() {
        let already_stored = [BTreeMap::from([("num_items", Value::BigInt(Some(1)))])];
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([already_stored])
            .into_connection();
        let warning = EvaluationWarning::UpstreamUnanswered {
            url: "https://cache.example",
        };

        warning
            .insert(&db, EvaluationId::now_v7())
            .await
            .expect("no insert follows the count");

        assert_eq!(db.into_transaction_log().len(), 1);
    }
}
