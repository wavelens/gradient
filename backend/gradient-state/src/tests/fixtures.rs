/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::StateConfiguration;

pub fn reporter_cfg(integration_name: &str, integrations_json: &str) -> StateConfiguration {
    let json = format!(
        r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
            }},
            "organizations": {{
                "acme": {{ "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice" }}
            }},
            "integrations": {integrations_json},
            "projects": {{
                "web": {{
                    "name": "web", "organization": "acme", "display_name": "Web",
                    "repository": "https://example.com/acme/web.git", "created_by": "alice",
                    "triggers": [
                        {{ "type": "reporter_push", "integration": "{integration_name}", "config": {{ "branches": ["main"] }} }}
                    ]
                }}
            }}
        }}"#
    );
    serde_json::from_str(&json).unwrap()
}

pub fn integration_cfg(integrations_json: &str) -> StateConfiguration {
    let json = format!(
        r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
            }},
            "organizations": {{
                "acme": {{ "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice" }}
            }},
            "integrations": {integrations_json}
        }}"#
    );
    serde_json::from_str(&json).unwrap()
}

pub fn worker_cfg(orgs_json: &str) -> StateConfiguration {
    let json = format!(
        r#"{{
            "users": {{
                "alice": {{
                    "username": "alice",
                    "name": "Alice",
                    "email": "alice@example.com",
                    "password_file": "/dev/null"
                }}
            }},
            "organizations": {{
                "acme": {{
                    "name": "acme",
                    "display_name": "ACME",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }},
                "globex": {{
                    "name": "globex",
                    "display_name": "Globex",
                    "private_key_file": "/dev/null",
                    "public": false,
                    "created_by": "alice"
                }}
            }},
            "workers": {{
                "builder-1": {{
                    "worker_id": "550e8400-e29b-41d4-a716-446655440001",
                    "organizations": {orgs_json},
                    "token_file": "/dev/null",
                    "display_name": "Primary Build Server",
                    "created_by": "alice"
                }}
            }}
        }}"#
    );
    serde_json::from_str(&json).unwrap()
}
