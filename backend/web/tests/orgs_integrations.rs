/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Integration tests for org-level integration endpoints.
//!
//! Focus: the credential-free summary endpoint
//! (`GET /orgs/{org}/integrations/summary`) used by the trigger UI. The
//! contract is:
//!
//! - any org member can call it (no `ManageIntegrations` required),
//! - response excludes `secret`, `endpoint_url`, `access_token`, and the
//!   `has_secret`/`has_access_token` booleans so non-admin members cannot
//!   probe credential state.

use entity::{ids::*, integration, organization_user};
use gradient_core::types::SessionId;
use sea_orm::{DatabaseBackend, MockDatabase};
use serde_json::Value;
use test_support::fixtures::{org, org_id, test_date, user, user_id};
use test_support::web::{live_session, make_test_server, make_token};
use uuid::Uuid;

// ── Fixtures ─────────────────────────────────────────────────────────────────

fn member_only_membership() -> organization_user::Model {
    // BASE_ROLE_VIEW grants ManageIntegrations today (see permissions::view_mask),
    // but the summary endpoint must work even without that - so we use a
    // synthetic role id that we never load. `OrgAccess::Member` does not
    // dereference the role.
    organization_user::Model {
        id: OrganizationUserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").unwrap(),
        ),
        organization: org_id(),
        user: user_id(),
        role: gradient_core::types::consts::BASE_ROLE_VIEW_ID,
    }
}

fn gitea_inbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000033").unwrap()),
        organization: org_id(),
        name: "my-gitea-hook".into(),
        display_name: "My Gitea".into(),
        kind: 0,       // Inbound
        forge_type: 0, // Gitea
        // Sensitive fields populated to verify they're NOT echoed back.
        secret: Some("encrypted-blob".into()),
        endpoint_url: None,
        access_token: None,
        allowed_ips: None,
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn github_inbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("019e16b2-e958-7652-ad97-67cd7b0fea61").unwrap()),
        organization: org_id(),
        name: "github".into(),
        display_name: "GitHub".into(),
        kind: 0,
        forge_type: 3,
        secret: None,
        endpoint_url: None,
        access_token: None,
        allowed_ips: None,
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn gitea_outbound_row() -> integration::Model {
    integration::Model {
        id: IntegrationId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000044").unwrap()),
        organization: org_id(),
        name: "my-gitea-reporter".into(),
        display_name: "My Gitea CI".into(),
        kind: 1,
        forge_type: 0,
        secret: None,
        endpoint_url: Some("https://gitea.example.com".into()),
        access_token: Some("encrypted-token".into()),
        allowed_ips: None,
        created_by: user_id(),
        created_at: test_date(),
    }
}

fn with_auth(db: MockDatabase, session_id: SessionId) -> MockDatabase {
    let session = live_session(session_id);
    db.append_query_results([vec![session.clone()]])
        .append_query_results([vec![session]])
        .append_query_results([vec![user()]])
}

/// `OrgAccess::Member` sequence: SELECT org, SELECT org_user. No role load.
fn with_org_member(db: MockDatabase) -> MockDatabase {
    db.append_query_results([vec![org()]])
        .append_query_results([vec![member_only_membership()]])
}

const SUMMARY_URL: &str = "/api/v1/orgs/test-org/integrations/summary";

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn summary_endpoint_returns_all_kinds() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_org_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![
            gitea_inbound_row(),
            github_inbound_row(),
            gitea_outbound_row(),
        ]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(SUMMARY_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        let items = body["message"].as_array().expect("array");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["kind"], "inbound");
        assert_eq!(items[0]["forge_type"], "gitea");
        assert_eq!(items[0]["name"], "my-gitea-hook");
        assert_eq!(items[1]["forge_type"], "github");
        assert_eq!(items[1]["display_name"], "GitHub");
        assert_eq!(items[2]["kind"], "outbound");
    });
}

#[test]
fn summary_endpoint_excludes_credential_state() {
    // Critical: the summary payload must not leak `secret`, `endpoint_url`,
    // `access_token`, `has_secret`, or `has_access_token`. Any of those would
    // let a non-admin org member fingerprint credentials.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_org_member(with_auth(
            MockDatabase::new(DatabaseBackend::Postgres),
            session_id,
        ))
        .append_query_results([vec![gitea_inbound_row(), gitea_outbound_row()]]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(SUMMARY_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_ok();
        let body: Value = res.json();
        for item in body["message"].as_array().unwrap() {
            let obj = item.as_object().unwrap();
            for forbidden in [
                "secret",
                "endpoint_url",
                "access_token",
                "has_secret",
                "has_access_token",
            ] {
                assert!(
                    !obj.contains_key(forbidden),
                    "summary leaked `{forbidden}`: {obj:?}"
                );
            }
        }
    });
}

#[test]
fn summary_endpoint_rejects_non_member() {
    // Non-member: the org_user lookup returns no row → 404 (Org loader hides
    // existence rather than returning 403).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let session_id = SessionId::now_v7();
        let token = make_token(session_id);

        let db = with_auth(MockDatabase::new(DatabaseBackend::Postgres), session_id)
            .append_query_results([vec![org()]])
            .append_query_results([Vec::<organization_user::Model>::new()]);

        let server = make_test_server(db.into_connection());
        let res = server
            .get(SUMMARY_URL)
            .add_header("authorization", format!("Bearer {}", token))
            .await;

        res.assert_status_not_found();
    });
}
