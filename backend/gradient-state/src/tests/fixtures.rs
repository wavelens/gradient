/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::StateConfiguration;

pub fn reporter_cfg(connection_name: &str, connections_json: &str) -> StateConfiguration {
    make_state_with_git_host_connections_and_web_task_extra_fields(
        connections_json,
        &format!(
            r#""git_host_connection": "{connection_name}",
            "triggers": [
                {{ "type": "reporter_push", "config": {{ "branches": ["main"] }} }}
            ]"#
        ),
    )
}

pub fn make_state_with_git_host_connections_and_web_task_extra_fields(
    connections_json: &str,
    task_fields_json: &str,
) -> StateConfiguration {
    let json = format!(
        r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
            }},
            "teams": {{
                "platform": {{ "name": "platform", "display_name": "Platform" }}
            }},
            "projects": {{
                "acme": {{ "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice", "owner_team": "platform" }}
            }},
            "git_host_connections": {connections_json},
            "tasks": {{
                "web": {{
                    "name": "web", "project": "acme", "display_name": "Web",
                    "repository": "https://example.com/acme/web.git", "created_by": "alice",
                    {task_fields_json}
                }}
            }}
        }}"#
    );
    serde_json::from_str(&json).unwrap()
}

pub fn state_with_one_user_one_project_and_git_host_connections(
    connections_json: &str,
) -> StateConfiguration {
    let json = format!(
        r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
            }},
            "projects": {{
                "acme": {{ "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice" }}
            }},
            "git_host_connections": {connections_json}
        }}"#
    );
    serde_json::from_str(&json).unwrap()
}

pub fn worker_cfg(projects_json: &str) -> StateConfiguration {
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
            "projects": {{
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
                    "projects": {projects_json},
                    "token_file": "/dev/null",
                    "display_name": "Primary Build Server",
                    "created_by": "alice"
                }}
            }}
        }}"#
    );
    serde_json::from_str(&json).unwrap()
}
