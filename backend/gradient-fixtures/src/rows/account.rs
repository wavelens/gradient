/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{Duration, Utc};
use gradient_entity::ids::SessionId;
use gradient_entity::{session, user};

use crate::ids::user_id;
use crate::test_date;

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
        active: true,
        ..Default::default()
    }
}

pub fn superuser() -> user::Model {
    user::Model {
        superuser: true,
        ..user()
    }
}

pub fn live_session(id: SessionId) -> session::Model {
    let now = Utc::now().naive_utc();
    session::Model {
        id,
        user_id: user_id(),
        created_at: now,
        expires_at: now + Duration::hours(1),
        last_used_at: now,
        ..Default::default()
    }
}
