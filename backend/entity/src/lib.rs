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
pub mod build_log_chunk;
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
pub mod cli_device_authorization;
pub mod commit;
pub mod derivation;
pub mod derivation_dependency;
pub mod derivation_feature;
pub mod derivation_metric;
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

pub mod acknowledged_derivation;
pub mod dispatched_job;
pub mod metric_rollup;
pub mod phase_event;
pub mod worker_connection;
pub mod worker_sample;

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

    #[test]
    fn phase_event_default_is_empty() {
        let m = phase_event::Model::default();
        assert_eq!(Uuid::from(m.id), Uuid::nil());
        assert_eq!(m.subject_kind, 0);
        assert!(m.worker_id.is_none());
        assert!(m.detail.is_none());
    }

    #[test]
    fn dispatched_job_default_has_empty_jsonb_and_zero_score() {
        let m = dispatched_job::Model::default();
        assert_eq!(Uuid::from(m.id), Uuid::nil());
        assert_eq!(m.score, 0.0);
        assert!(m.build_id.is_none());
        assert!(m.finished_at.is_none());
    }

    #[test]
    fn worker_sample_default_has_zero_counts() {
        let m = worker_sample::Model::default();
        assert_eq!(m.assigned_jobs, 0);
        assert_eq!(m.state, 0);
        assert!(m.cpu_usage_pct.is_none());
    }

    #[test]
    fn worker_connection_default_is_open() {
        let m = worker_connection::Model::default();
        assert!(m.disconnected_at.is_none());
        assert!(m.reason.is_none());
    }

    #[test]
    fn acknowledged_derivation_default_blank() {
        let m = acknowledged_derivation::Model::default();
        assert!(m.derivation.is_none());
        assert!(m.pname.is_none());
        assert_eq!(m.note, "");
    }

    #[test]
    fn metric_rollup_default_zeroed() {
        let m = metric_rollup::Model::default();
        assert_eq!(m.metric, "");
        assert_eq!(m.granularity, 0);
        assert_eq!(m.count, 0);
        assert_eq!(m.sum, 0.0);
        assert!(m.histogram.is_none());
    }

    #[test]
    fn build_default_has_null_phase_timestamps() {
        let m = build::Model::default();
        assert!(m.ready_at.is_none());
        assert!(m.dispatched_at.is_none());
        assert!(m.build_started_at.is_none());
        assert!(m.build_finished_at.is_none());
    }

    #[test]
    fn evaluation_default_has_null_phase_timestamps() {
        let m = evaluation::Model::default();
        assert!(m.fetch_started_at.is_none());
        assert!(m.eval_flake_started_at.is_none());
        assert!(m.eval_drv_started_at.is_none());
        assert!(m.building_started_at.is_none());
        assert!(m.finished_at.is_none());
    }
}
