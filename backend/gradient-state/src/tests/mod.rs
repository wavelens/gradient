/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::{StateConfiguration, resolve_oidc_group_roles, resolve_scim_group_roles};
use fixtures::{integration_cfg, reporter_cfg, worker_cfg};
use gradient_types::{ProjectId, RoleId};
use std::collections::HashMap;

#[test]
fn project_task_cache_descriptions_optional() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice",
                "name": "Alice",
                "email": "alice@example.com",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "description": null,
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web",
                "project": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice"
            }
        },
        "caches": {
            "main": {
                "name": "main",
                "display_name": "Main",
                "signing_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert!(cfg.projects["acme"].description.is_none());
    assert!(cfg.tasks["web"].description.is_none());
    assert!(cfg.caches["main"].description.is_none());
    assert!(cfg.validate().is_valid);
}

#[test]
fn state_task_accepts_legacy_evaluation_wildcard_alias() {
    // Existing nix configurations using `evaluation_wildcard` must keep
    // working after the rename to `wildcard`.
    let json = r#"{
        "tasks": {
            "web": {
                "name": "web",
                "project": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "evaluation_wildcard": "checks.*",
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.tasks["web"].wildcard, "checks.*");
}

#[test]
fn state_task_keep_evaluations_zero_rejected_by_validator() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice",
                "name": "Alice",
                "email": "alice@example.com",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web",
                "project": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "keep_evaluations": 0
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "tasks.web.keep_evaluations" && e.message.contains("at least 1")),
        "expected keep_evaluations >= 1 validation error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_reporter_trigger_accepts_declared_inbound_integration() {
    let integrations = r#"{
        "git-host": { "name": "git-host", "project": "acme", "kind": "inbound", "git_host_type": "forgejo", "created_by": "alice" }
    }"#;
    let cfg = reporter_cfg("git-host", integrations);
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_reporter_trigger_rejects_unknown_integration() {
    let cfg = reporter_cfg("ghost", "{}");
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "tasks.web.triggers" && e.message.contains("ghost")),
        "expected unknown-integration trigger error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_reporter_trigger_rejects_outbound_integration() {
    let integrations = r#"{
        "git-host": { "name": "git-host", "project": "acme", "kind": "outbound", "git_host_type": "forgejo", "created_by": "alice" }
    }"#;
    let cfg = reporter_cfg("git-host", integrations);
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(|e| e.field == "tasks.web.triggers"),
        "expected outbound-integration trigger error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_reporter_trigger_accepts_github_app_name() {
    let cfg = reporter_cfg("github", "{}");
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_github_integration_requires_installation_id() {
    let integrations = r#"{
        "gh": { "name": "gh", "project": "acme", "kind": "outbound", "git_host_type": "github", "created_by": "alice" }
    }"#;
    let cfg = integration_cfg(integrations);
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "integrations.gh.installation_id"),
        "expected installation_id requirement, got: {:?}",
        v.errors
    );
}

#[test]
fn state_github_integration_with_installation_id_is_valid() {
    let integrations = r#"{
        "gh": { "name": "gh", "project": "acme", "kind": "outbound", "git_host_type": "github", "installation_id": 42, "account_login": "acme", "created_by": "alice" }
    }"#;
    let cfg = integration_cfg(integrations);
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_action_rejects_unknown_field() {
    let json = r#"{
        "tasks": {
            "web": {
                "name": "web",
                "project": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "actions": [
                    {
                        "name": "x",
                        "type": "send_mail",
                        "events": [],
                        "config": {},
                        "bogus": true
                    }
                ]
            }
        }
    }"#;
    let err = serde_json::from_str::<StateConfiguration>(json).unwrap_err();
    assert!(err.to_string().contains("bogus"), "got: {err}");
}

#[test]
fn state_action_validate_rejects_unknown_type() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice", "name": "Alice", "email": "a@x.io",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web", "project": "acme", "display_name": "Web",
                "repository": "https://example.com/acme/web.git", "created_by": "alice",
                "actions": [
                    { "name": "a", "type": "garbage", "config": {} }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "tasks.web.actions.a.type"),
        "expected unknown-type error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_action_validate_rejects_duplicate_names() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice", "name": "Alice", "email": "a@x.io",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web", "project": "acme", "display_name": "Web",
                "repository": "https://example.com/acme/web.git", "created_by": "alice",
                "actions": [
                    { "name": "dup", "type": "send_mail", "config": { "recipients": ["a@x.io"] } },
                    { "name": "dup", "type": "send_mail", "config": { "recipients": ["b@x.io"] } }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.message.contains("Duplicate action name")),
        "expected duplicate-name error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_action_validate_rejects_events_on_git_host_status_report() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice", "name": "Alice", "email": "a@x.io",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web", "project": "acme", "display_name": "Web",
                "repository": "https://example.com/acme/web.git", "created_by": "alice",
                "actions": [
                    { "name": "x", "type": "git_host_status_report", "events": ["build.completed"], "config": { "integration": "gh" } }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "tasks.web.actions.x.events"),
        "expected git_host_status_report-events error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_action_validate_accepts_open_pr() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice", "name": "Alice", "email": "a@x.io",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web", "project": "acme", "display_name": "Web",
                "repository": "https://example.com/acme/web.git", "created_by": "alice",
                "actions": [
                    { "name": "flake-update", "type": "open_pr", "config": { "integration": "gh" } }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.is_valid,
        "open_pr must be a valid action type, got: {:?}",
        v.errors
    );
}

#[test]
fn state_action_validate_rejects_events_on_open_pr() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice", "name": "Alice", "email": "a@x.io",
                "password_file": "/dev/null"
            }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "tasks": {
            "web": {
                "name": "web", "project": "acme", "display_name": "Web",
                "repository": "https://example.com/acme/web.git", "created_by": "alice",
                "actions": [
                    { "name": "x", "type": "open_pr", "events": ["build.completed"], "config": { "integration": "gh" } }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "tasks.web.actions.x.events"),
        "expected open_pr-events error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_task_silently_ignores_legacy_force_evaluation_field() {
    // Old state files may still set `force_evaluation` - serde drops
    // unknown fields by default, so parsing must keep working.
    let json = r#"{
        "tasks": {
            "web": {
                "name": "web",
                "project": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "force_evaluation": true
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.tasks["web"].name, "web");
}

#[test]
fn state_worker_accepts_multiple_projects() {
    let cfg = worker_cfg(r#"["acme", "globex"]"#);
    assert_eq!(
        cfg.workers["builder-1"].projects,
        vec!["acme".to_owned(), "globex".to_owned()]
    );
    assert!(cfg.validate().is_valid);
}

#[test]
fn state_worker_rejects_empty_projects() {
    let cfg = worker_cfg("[]");
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(
            |e| e.field == "workers.550e8400-e29b-41d4-a716-446655440001.projects"
                && e.message.contains("at least one")
        ),
        "expected at-least-one-project error, got: {:?}",
        v.errors
    );
}

fn base_worker_cfg(authorize_against: &str) -> StateConfiguration {
    let json = format!(
        r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "alice@example.com", "password_file": "/dev/null" }}
            }},
            "workers": {{
                "base-1": {{
                    "worker_id": "550e8400-e29b-41d4-a716-446655440001",
                    "projects": [],
                    "token_file": "/dev/null",
                    "display_name": "Base Build Server",
                    "created_by": "alice",
                    "base_worker": true,
                    "authorize_against": {authorize_against}
                }}
            }}
        }}"#
    );

    serde_json::from_str(&json).unwrap()
}

#[test]
fn base_worker_rejects_bad_authorize_against() {
    let cfg = base_worker_cfg(r#""not-a-uuid""#);
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.message.contains("authorize_against")),
        "expected authorize_against error, got: {:?}",
        v.errors
    );
}

#[test]
fn base_worker_accepts_valid_authorize_against_and_empty_projects() {
    let cfg = base_worker_cfg(r#""018f6f3a-0000-7000-8000-000000000001""#);
    assert!(cfg.validate().is_valid, "{:?}", cfg.validate().errors);
}

#[test]
fn state_project_validator_rejects_malformed_id() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME", "id": "not-a-uuid",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(|e| e.field == "projects.acme.id"),
        "expected invalid-id error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_project_validator_rejects_duplicate_ids() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME", "id": "018f6f3a-0000-7000-8000-000000000001",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            },
            "globex": {
                "name": "globex", "display_name": "Globex", "id": "018f6f3a-0000-7000-8000-000000000001",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.message.contains("Duplicate project id")),
        "expected duplicate-id error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_project_members_validator_accepts_builtin_role() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" },
            "bob":   { "username": "bob",   "name": "Bob",   "email": "b@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                "members": [{ "user": "bob", "role": "Write" }]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_project_members_validator_accepts_custom_project_role() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                "members": [{ "user": "alice", "role": "releaser" }]
            }
        },
        "roles": {
            "releaser": { "name": "releaser", "project": "acme", "permissions": ["viewProject"] }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_project_members_validator_rejects_unknown_role() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                "members": [{ "user": "alice", "role": "Ghost" }]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "projects.acme.members.alice.role"),
        "expected unknown-role error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_project_members_validator_ignores_unknown_user() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                "members": [{ "user": "ghost", "role": "Write" }]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.is_valid,
        "missing user must not fail validation (issue #94): {:?}",
        v.errors
    );
}

#[test]
fn state_project_members_validator_rejects_duplicate_user() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                "members": [
                    { "user": "alice", "role": "Write" },
                    { "user": "alice", "role": "View" }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors
            .iter()
            .any(|e| e.message.contains("Duplicate member")),
        "expected duplicate-member error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_worker_rejects_unknown_project_in_list() {
    let cfg = worker_cfg(r#"["acme", "ghost"]"#);
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(
            |e| e.field == "workers.550e8400-e29b-41d4-a716-446655440001.projects"
                && e.message.contains("'ghost'")
        ),
        "expected unknown-project error mentioning 'ghost', got: {:?}",
        v.errors
    );
}

#[test]
fn resolves_group_to_project_role_grants() {
    let json = r#"{
        "roles": {
            "platform": {
                "name": "platform-admin",
                "project": "acme",
                "permissions": ["create_task"],
                "oidc_group": ["platform-team", "ops"]
            },
            "unmapped": {
                "name": "viewer",
                "project": "acme",
                "permissions": ["view_task"]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();

    let project = ProjectId::now_v7();
    let role = RoleId::now_v7();
    let mut role_ids = HashMap::new();
    role_ids.insert(
        ("acme".to_string(), "platform-admin".to_string()),
        (project, role),
    );
    role_ids.insert(
        ("acme".to_string(), "viewer".to_string()),
        (project, RoleId::now_v7()),
    );

    let resolved = resolve_oidc_group_roles(&cfg, &role_ids);
    assert_eq!(resolved.get("platform-team"), Some(&vec![(project, role)]));
    assert_eq!(resolved.get("ops"), Some(&vec![(project, role)]));
    assert!(!resolved.contains_key("unmapped"));
}

#[test]
fn resolves_scim_group_to_project_role_grants() {
    let json = r#"{
        "roles": {
            "eng": {
                "name": "platform-admin",
                "project": "acme",
                "permissions": ["create_task"],
                "scim_group": ["acme-eng", "ops"]
            },
            "unmapped": {
                "name": "viewer",
                "project": "acme",
                "permissions": ["view_task"]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();

    let project = ProjectId::now_v7();
    let role = RoleId::now_v7();
    let mut role_ids = HashMap::new();
    role_ids.insert(
        ("acme".to_string(), "platform-admin".to_string()),
        (project, role),
    );
    role_ids.insert(
        ("acme".to_string(), "viewer".to_string()),
        (project, RoleId::now_v7()),
    );

    let resolved = resolve_scim_group_roles(&cfg, &role_ids);
    assert_eq!(resolved.get("acme-eng"), Some(&vec![(project, role)]));
    assert_eq!(resolved.get("ops"), Some(&vec![(project, role)]));
    assert!(!resolved.contains_key("unmapped"));
}

#[test]
fn state_worker_accepts_missing_created_by() {
    // A host that provisions a worker for itself has no declared user to
    // attribute it to, so `created_by` must be optional and validate clean.
    let json = r#"{
        "workers": {
            "local": {
                "worker_id": "550e8400-e29b-41d4-a716-446655440099",
                "projects": [],
                "token_file": "/dev/null",
                "display_name": "Local Worker",
                "base_worker": true,
                "auto_enable": true
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert!(cfg.workers["local"].created_by.is_none());
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_worker_rejects_unknown_created_by() {
    let json = r#"{
        "workers": {
            "builder-1": {
                "worker_id": "550e8400-e29b-41d4-a716-446655440001",
                "projects": [],
                "token_file": "/dev/null",
                "display_name": "Builder",
                "base_worker": true,
                "created_by": "ghost"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(|e| e
            .field
            .ends_with("550e8400-e29b-41d4-a716-446655440001.created_by")
            && e.message.contains("ghost")),
        "expected unknown created_by error, got: {:?}",
        v.errors
    );
}
