/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Deterministic fixture builders. Stable UUIDs make assertions readable:
//! you can write `assert_eq!(body["name"], "test-org")` instead of chasing
//! a random `Uuid`.

use entity::*;
use uuid::Uuid;

pub fn org_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
}
pub fn user_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
}
pub fn project_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()
}
pub fn commit_id() -> Uuid {
    Uuid::parse_str("00000000-0000-0000-0000-000000000013").unwrap()
}

pub fn test_date() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
}

pub fn org() -> organization::Model {
    organization::Model {
        id: org_id(),
        name: "test-org".into(),
        display_name: "Test Organization".into(),
        description: "".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        use_nix_store: false,
        public: false,
        created_by: user_id(),
        created_at: test_date(),
        managed: false,
    }
}

pub fn user() -> user::Model {
    user::Model {
        id: user_id(),
        username: "testuser".into(),
        name: "Test User".into(),
        email: "test@example.com".into(),
        password: Some(password_auth::generate_hash("TestPass123!")),
        last_login_at: test_date(),
        created_at: test_date(),
        email_verified: true,
        email_verification_token: None,
        email_verification_token_expires: None,
        managed: false,
    }
}

pub fn eval_at(id: Uuid, offset_secs: i64) -> evaluation::Model {
    let created_at = test_date() + chrono::Duration::seconds(offset_secs);
    evaluation::Model {
        id,
        project: Some(project_id()),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status: evaluation::EvaluationStatus::Completed,
        previous: None,
        next: None,
        created_at,
        updated_at: created_at,
        error: None,
    }
}
