/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::{StateConfiguration, resolve_oidc_group_roles, resolve_scim_group_roles};
use gradient_types::triggers::ConcurrencyPolicy;
use gradient_types::{OrganizationId, RoleId};
use fixtures::{reporter_cfg, worker_cfg};
use std::collections::HashMap;

#[test]
fn user_accepts_missing_password_file() {
    // OIDC-only users have `password_file = null`; serde must default to
    // None instead of failing. This is the on-disk contract that lets
    // gradient-state.nix emit `password_file = null` entries.
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice",
                "name": "Alice",
                "email": "alice@example.com",
                "password_file": null,
                "email_verified": true,
                "superuser": false
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert!(cfg.users["alice"].password_file.is_none());
}

#[test]
fn org_project_cache_descriptions_optional() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice",
                "name": "Alice",
                "email": "alice@example.com",
                "password_file": "/dev/null"
            }
        },
        "organizations": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "description": null,
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        },
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
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
    assert!(cfg.organizations["acme"].description.is_none());
    assert!(cfg.projects["web"].description.is_none());
    assert!(cfg.caches["main"].description.is_none());
    assert!(cfg.validate().is_valid);
}

#[test]
fn state_project_concurrency_defaults_to_soft_abort() {
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].concurrency, ConcurrencyPolicy::SoftAbort);
}

#[test]
fn state_project_accepts_wildcard_field() {
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "wildcard": "packages.x86_64-linux.*",
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].wildcard, "packages.x86_64-linux.*");
}

#[test]
fn state_project_accepts_legacy_evaluation_wildcard_alias() {
    // Existing nix configurations using `evaluation_wildcard` must keep
    // working after the rename to `wildcard`.
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "evaluation_wildcard": "checks.*",
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].wildcard, "checks.*");
}

#[test]
fn state_project_keep_evaluations_defaults_to_thirty() {
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].keep_evaluations, 30);
}

#[test]
fn state_project_keep_evaluations_zero_rejected_by_validator() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice",
                "name": "Alice",
                "email": "alice@example.com",
                "password_file": "/dev/null"
            }
        },
        "organizations": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        },
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
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
            .any(|e| e.field == "projects.web.keep_evaluations"
                && e.message.contains("at least 1")),
        "expected keep_evaluations >= 1 validation error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_project_actions_round_trip_all_types() {
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "actions": [
                    {
                        "name": "notify-ops",
                        "type": "send_mail",
                        "events": ["build.failed"],
                        "config": { "recipients": ["ops@example.com"] }
                    },
                    {
                        "name": "webhook",
                        "type": "send_web_request",
                        "events": ["build.completed"],
                        "config": { "url": "https://hooks.example.com/gradient", "token_file": "/etc/gradient/secrets/hook-token" }
                    },
                    {
                        "name": "status",
                        "type": "forge_status_report",
                        "config": { "integration": "gitea-prod" }
                    }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].actions.len(), 3);
    assert_eq!(cfg.projects["web"].actions[0].action_type, "send_mail");
    assert!(cfg.projects["web"].actions[2].events.is_empty());
}

#[test]
fn state_reporter_trigger_accepts_declared_inbound_integration() {
    let integrations = r#"{
        "forge": { "name": "forge", "organization": "acme", "kind": "inbound", "forge_type": "forgejo", "created_by": "alice" }
    }"#;
    let cfg = reporter_cfg("forge", integrations);
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
            .any(|e| e.field == "projects.web.triggers" && e.message.contains("ghost")),
        "expected unknown-integration trigger error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_reporter_trigger_rejects_outbound_integration() {
    let integrations = r#"{
        "forge": { "name": "forge", "organization": "acme", "kind": "outbound", "forge_type": "forgejo", "created_by": "alice" }
    }"#;
    let cfg = reporter_cfg("forge", integrations);
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(|e| e.field == "projects.web.triggers"),
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
fn state_action_rejects_unknown_field() {
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
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
        "organizations": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "projects": {
            "web": {
                "name": "web", "organization": "acme", "display_name": "Web",
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
            .any(|e| e.field == "projects.web.actions.a.type"),
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
        "organizations": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "projects": {
            "web": {
                "name": "web", "organization": "acme", "display_name": "Web",
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
fn state_action_validate_rejects_events_on_forge_status_report() {
    let json = r#"{
        "users": {
            "alice": {
                "username": "alice", "name": "Alice", "email": "a@x.io",
                "password_file": "/dev/null"
            }
        },
        "organizations": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice"
            }
        },
        "projects": {
            "web": {
                "name": "web", "organization": "acme", "display_name": "Web",
                "repository": "https://example.com/acme/web.git", "created_by": "alice",
                "actions": [
                    { "name": "x", "type": "forge_status_report", "events": ["build.completed"], "config": { "integration": "gh" } }
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
            .any(|e| e.field == "projects.web.actions.x.events"),
        "expected forge_status_report-events error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_project_silently_ignores_legacy_force_evaluation_field() {
    // Old state files may still set `force_evaluation` - serde drops
    // unknown fields by default, so parsing must keep working.
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "force_evaluation": true
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].name, "web");
}

#[test]
fn state_project_concurrency_hard_abort_round_trip() {
    let json = r#"{
        "projects": {
            "web": {
                "name": "web",
                "organization": "acme",
                "display_name": "Web",
                "repository": "https://example.com/acme/web.git",
                "created_by": "alice",
                "concurrency": "hard_abort"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.projects["web"].concurrency, ConcurrencyPolicy::HardAbort);
    assert_eq!(i16::from(cfg.projects["web"].concurrency), 0);
}

#[test]
fn state_worker_accepts_multiple_organizations() {
    let cfg = worker_cfg(r#"["acme", "globex"]"#);
    assert_eq!(
        cfg.workers["builder-1"].organizations,
        vec!["acme".to_owned(), "globex".to_owned()]
    );
    assert!(cfg.validate().is_valid);
}

#[test]
fn state_worker_rejects_empty_organizations() {
    let cfg = worker_cfg("[]");
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(|e| e.field
            == "workers.550e8400-e29b-41d4-a716-446655440001.organizations"
            && e.message.contains("at least one")),
        "expected at-least-one-org error, got: {:?}",
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
                    "organizations": [],
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
        v.errors.iter().any(|e| e.message.contains("authorize_against")),
        "expected authorize_against error, got: {:?}",
        v.errors
    );
}

#[test]
fn base_worker_accepts_valid_authorize_against_and_empty_orgs() {
    let cfg = base_worker_cfg(r#""018f6f3a-0000-7000-8000-000000000001""#);
    assert!(cfg.validate().is_valid, "{:?}", cfg.validate().errors);
}

#[test]
fn state_org_accepts_explicit_id() {
    let json = r#"{
        "organizations": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "id": "018f6f3a-0000-7000-8000-000000000001",
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert_eq!(
        cfg.organizations["acme"].id.as_deref(),
        Some("018f6f3a-0000-7000-8000-000000000001")
    );
}

#[test]
fn state_org_id_defaults_none() {
    let json = r#"{
        "organizations": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert!(cfg.organizations["acme"].id.is_none());
}

#[test]
fn state_org_validator_rejects_malformed_id() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
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
        v.errors.iter().any(|e| e.field == "organizations.acme.id"),
        "expected invalid-id error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_org_validator_rejects_duplicate_ids() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
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
            .any(|e| e.message.contains("Duplicate organization id")),
        "expected duplicate-id error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_org_members_serde_round_trip() {
    let json = r#"{
        "organizations": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice",
                "members": [
                    { "user": "bob", "role": "Write" },
                    { "user": "carol", "role": "releaser" }
                ]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let members = &cfg.organizations["acme"].members;
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].user, "bob");
    assert_eq!(members[0].role, "Write");
    assert_eq!(members[1].user, "carol");
    assert_eq!(members[1].role, "releaser");
}

#[test]
fn state_org_members_default_empty() {
    let json = r#"{
        "organizations": {
            "acme": {
                "name": "acme",
                "display_name": "ACME",
                "private_key_file": "/dev/null",
                "public": false,
                "created_by": "alice"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    assert!(cfg.organizations["acme"].members.is_empty());
}

#[test]
fn state_org_members_validator_accepts_builtin_role() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" },
            "bob":   { "username": "bob",   "name": "Bob",   "email": "b@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
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
fn state_org_members_validator_accepts_custom_org_role() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
            "acme": {
                "name": "acme", "display_name": "ACME",
                "private_key_file": "/dev/null", "public": false, "created_by": "alice",
                "members": [{ "user": "alice", "role": "releaser" }]
            }
        },
        "roles": {
            "releaser": { "name": "releaser", "organization": "acme", "permissions": ["viewOrg"] }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_org_members_validator_rejects_unknown_role() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
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
            .any(|e| e.field == "organizations.acme.members.alice.role"),
        "expected unknown-role error, got: {:?}",
        v.errors
    );
}

#[test]
fn state_org_members_validator_ignores_unknown_user() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
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
fn state_org_members_validator_rejects_duplicate_user() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "organizations": {
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
fn state_worker_rejects_unknown_organization_in_list() {
    let cfg = worker_cfg(r#"["acme", "ghost"]"#);
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(|e| e.field
            == "workers.550e8400-e29b-41d4-a716-446655440001.organizations"
            && e.message.contains("'ghost'")),
        "expected unknown-org error mentioning 'ghost', got: {:?}",
        v.errors
    );
}

#[test]
fn resolves_group_to_org_role_grants() {
    let json = r#"{
        "roles": {
            "platform": {
                "name": "platform-admin",
                "organization": "acme",
                "permissions": ["create_project"],
                "oidc_group": ["platform-team", "ops"]
            },
            "unmapped": {
                "name": "viewer",
                "organization": "acme",
                "permissions": ["view_project"]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();

    let org = OrganizationId::now_v7();
    let role = RoleId::now_v7();
    let mut role_ids = HashMap::new();
    role_ids.insert(("acme".to_string(), "platform-admin".to_string()), (org, role));
    role_ids.insert(
        ("acme".to_string(), "viewer".to_string()),
        (org, RoleId::now_v7()),
    );

    let resolved = resolve_oidc_group_roles(&cfg, &role_ids);
    assert_eq!(resolved.get("platform-team"), Some(&vec![(org, role)]));
    assert_eq!(resolved.get("ops"), Some(&vec![(org, role)]));
    assert!(!resolved.contains_key("unmapped"));
}

#[test]
fn resolves_scim_group_to_org_role_grants() {
    let json = r#"{
        "roles": {
            "eng": {
                "name": "platform-admin",
                "organization": "acme",
                "permissions": ["create_project"],
                "scim_group": ["acme-eng", "ops"]
            },
            "unmapped": {
                "name": "viewer",
                "organization": "acme",
                "permissions": ["view_project"]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();

    let org = OrganizationId::now_v7();
    let role = RoleId::now_v7();
    let mut role_ids = HashMap::new();
    role_ids.insert(("acme".to_string(), "platform-admin".to_string()), (org, role));
    role_ids.insert(
        ("acme".to_string(), "viewer".to_string()),
        (org, RoleId::now_v7()),
    );

    let resolved = resolve_scim_group_roles(&cfg, &role_ids);
    assert_eq!(resolved.get("acme-eng"), Some(&vec![(org, role)]));
    assert_eq!(resolved.get("ops"), Some(&vec![(org, role)]));
    assert!(!resolved.contains_key("unmapped"));
}
