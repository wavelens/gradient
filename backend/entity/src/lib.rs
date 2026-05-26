/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod ids;

pub mod admin_task;
pub mod api;
pub mod audit_log;
pub mod build;
pub mod build_product;
pub mod build_request_blob;
pub mod cache;
pub mod cache_derivation;
pub mod cache_metric;
pub mod cache_role;
pub mod cache_upstream;
pub mod cache_user;
pub mod cached_path;
pub mod cached_path_signature;
pub mod commit;
pub mod derivation;
pub mod derivation_dependency;
pub mod derivation_feature;
pub mod derivation_output;
pub mod entry_point;
pub mod entry_point_message;
pub mod evaluation;
pub mod evaluation_flake_input_override;
pub mod evaluation_message;
pub mod feature;
pub mod integration;
pub mod organization;
pub mod organization_cache;
pub mod organization_user;
pub mod project;
pub mod project_action;
pub mod project_action_delivery;
pub mod project_flake_input_override;
pub mod project_trigger;
pub mod role;
pub mod server;
pub mod session;
pub mod upload_session;
pub mod user;
pub mod worker_registration;

#[cfg(test)]
mod model_default_tests {
    use super::*;
    use chrono::NaiveDateTime;
    use uuid::Uuid;

    #[test]
    fn user_default_has_nil_id_and_blank_strings() {
        let m = user::Model::default();
        assert_eq!(Uuid::from(m.id), Uuid::nil());
        assert_eq!(m.username, "");
        assert!(!m.email_verified);
        assert!(m.password.is_none());
    }

    #[test]
    fn build_default_has_initial_status() {
        let m = build::Model::default();
        assert_eq!(m.status, build::BuildStatus::Created);
        assert!(!m.external_cached);
    }

    #[test]
    fn evaluation_default_has_initial_status() {
        let m = evaluation::Model::default();
        assert_eq!(m.status, evaluation::EvaluationStatus::Queued);
        assert!(m.check_run_ids.is_none());
    }

    #[test]
    fn audit_log_default_has_null_json() {
        let m = audit_log::Model::default();
        assert!(m.metadata.is_none());
        assert_eq!(m.created_at, NaiveDateTime::default());
    }

    #[test]
    fn organization_cache_default_uses_read_write_mode() {
        let m = organization_cache::Model::default();
        assert_eq!(m.mode, organization_cache::CacheSubscriptionMode::ReadWrite);
    }
}
