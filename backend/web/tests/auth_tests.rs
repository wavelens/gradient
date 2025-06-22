/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod common;

use web::endpoints::auth::*;

#[test]
fn test_make_login_request_serialization() {
    let request = MakeLoginRequest {
        loginname: "testuser".to_string(),
        password: "password123".to_string(),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("testuser"));
    assert!(json.contains("password123"));
}

#[test]
fn test_make_user_request_serialization() {
    let request = MakeUserRequest {
        username: "testuser".to_string(),
        name: "Test User".to_string(),
        email: "test@example.com".to_string(),
        password: "password123".to_string(),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("testuser"));
    assert!(json.contains("Test User"));
    assert!(json.contains("test@example.com"));
}
