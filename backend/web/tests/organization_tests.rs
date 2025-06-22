/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod common;

use web::endpoints::orgs::*;

#[test]
fn test_make_organization_request_serialization() {
    let request = MakeOrganizationRequest {
        name: "test-org".to_string(),
        display_name: "Test Organization".to_string(),
        description: "A test organization".to_string(),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("test-org"));
    assert!(json.contains("Test Organization"));
    assert!(json.contains("A test organization"));
}

#[test]
fn test_add_user_request_serialization() {
    let request = AddUserRequest {
        user: "testuser".to_string(),
        role: "admin".to_string(),
    };

    let json = serde_json::to_string(&request).unwrap();
    assert!(json.contains("testuser"));
    assert!(json.contains("admin"));
}
