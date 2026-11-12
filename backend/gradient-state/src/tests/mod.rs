/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod fixtures;

use super::StateConfiguration;
use fixtures::{
    make_state_with_git_host_connections_and_web_task_extra_fields, reporter_cfg,
    state_with_one_user_one_project_and_git_host_connections, worker_cfg,
};

const WITHOUT_OPTIONAL_FIELDS: &str = r#"{
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

#[test]
fn project_task_cache_descriptions_optional() {
    let cfg: StateConfiguration = serde_json::from_str(WITHOUT_OPTIONAL_FIELDS).unwrap();
    assert!(cfg.projects["acme"].description.is_none());
    assert!(cfg.tasks["web"].description.is_none());
    assert!(cfg.caches["main"].description.is_none());
    assert!(cfg.validate().is_valid);
}

#[test]
fn a_state_task_with_an_escaping_flake_directory_is_refused() {
    let with_repository = |repository: &str| {
        let mut cfg: StateConfiguration = serde_json::from_str(WITHOUT_OPTIONAL_FIELDS).unwrap();
        cfg.tasks.get_mut("web").unwrap().repository = repository.to_owned();
        cfg.validate()
    };
    let escaping = with_repository("https://github.com/acme/infra?dir=../x");
    let fields: Vec<&str> = escaping.errors.iter().map(|e| e.field.as_str()).collect();
    assert_eq!(fields, ["tasks.web.repository"]);
    assert!(with_repository("https://github.com/acme/infra?dir=nix").is_valid);
}

#[test]
fn secret_files_default_to_empty() {
    let cfg: StateConfiguration = serde_json::from_str(WITHOUT_OPTIONAL_FIELDS).unwrap();
    assert!(cfg.projects["acme"].secret_files.is_empty());
}

#[test]
fn a_task_without_exec_enabled_keeps_exec_jobs_off() {
    let cfg: StateConfiguration = serde_json::from_str(WITHOUT_OPTIONAL_FIELDS).unwrap();
    assert!(!cfg.tasks["web"].exec_enabled);
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
fn state_task_with_two_default_triggers_rejected() {
    let mut cfg = reporter_cfg("github", "{}");
    let task = cfg.tasks.get_mut("web").unwrap();
    let triggers = task.triggers.as_mut().unwrap();
    triggers[0].is_default = true;
    triggers.push(crate::StateTrigger::default_polling());
    let v = cfg.validate();
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "tasks.web.triggers" && e.message.contains("is_default")),
        "expected a second default trigger to be rejected, got: {:?}",
        v.errors
    );
}

fn errors_of(cfg: &StateConfiguration) -> Vec<(String, String)> {
    cfg.validate()
        .errors
        .into_iter()
        .map(|e| (e.field, e.message))
        .collect()
}

#[test]
fn state_validation_rejects_extra_allowed_url_prefix_with_whitespace() {
    let mut cfg = reporter_cfg("github", "{}");
    let project = cfg.projects.get_mut("acme").unwrap();
    assert!(project.restrict_evaluation_fetches);
    project.extra_allowed_uri_prefixes = vec!["github:".into(), "https://a https://b".into()];

    let v = cfg.validate();

    let rejected: Vec<_> = v
        .errors
        .iter()
        .filter(|e| e.field == "projects.acme.extra_allowed_uri_prefixes")
        .collect();
    assert_eq!(rejected.len(), 1, "{:?}", v.errors);
}

#[test]
fn state_reporter_trigger_accepts_declared_receiving_connection() {
    let connections = r#"{
        "git-host": { "name": "git-host", "owner": "project/acme", "git_host_type": "forgejo", "created_by": "alice" }
    }"#;
    let cfg = reporter_cfg("git-host", connections);
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_reporter_trigger_rejects_unknown_connection() {
    let cfg = reporter_cfg("ghost", "{}");
    let errors = errors_of(&cfg);
    assert!(
        errors
            .iter()
            .any(|(field, message)| field == "tasks.web.git_host_connection"
                && message.contains("ghost")),
        "expected unknown-connection error, got: {errors:?}"
    );
}

#[test]
fn push_trigger_rejects_missing_or_non_receiving_connection() {
    let deaf = r#"{
        "git-host": { "name": "git-host", "owner": "project/acme", "git_host_type": "forgejo", "receives_events": false, "created_by": "alice" }
    }"#;
    let errors = errors_of(&reporter_cfg("git-host", deaf));
    assert!(
        errors
            .iter()
            .any(|(field, message)| field == "tasks.web.triggers"
                && message.contains("receives_events")),
        "expected receives_events error, got: {errors:?}"
    );

    let without_connection = make_state_with_git_host_connections_and_web_task_extra_fields(
        "{}",
        r#""triggers": [{ "type": "reporter_pull_request" }]"#,
    );
    let errors = errors_of(&without_connection);
    assert!(
        errors
            .iter()
            .any(|(field, message)| field == "tasks.web.triggers"
                && message.contains("git_host_connection")),
        "expected missing-connection error, got: {errors:?}"
    );
}

#[test]
fn state_reporter_trigger_accepts_github_app_name() {
    let cfg = reporter_cfg("github-acme", "{}");
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn task_connection_name_resolves_to_project_before_owner_team_before_instance() {
    let connection = |owner: &str, reports_status: bool| {
        format!(
            r#"{{ "name": "forge", "owner": "{owner}", "git_host_type": "gitea", "reports_status": {reports_status}, "created_by": "alice" }}"#
        )
    };
    let reporting = r#""git_host_connection": "forge", "report_status": true"#;
    let validate = |connections: &[(&str, String)]| {
        let entries: Vec<String> = connections
            .iter()
            .map(|(label, connection)| format!(r#""{label}": {connection}"#))
            .collect();
        let cfg = make_state_with_git_host_connections_and_web_task_extra_fields(
            &format!("{{ {} }}", entries.join(", ")),
            reporting,
        );
        cfg.validate().is_valid
    };

    let instance_reports = ("instance", connection("instance", true));
    let team_silent = ("team", connection("team/platform", false));
    let project_reports = ("project", connection("project/acme", true));

    assert!(validate(std::slice::from_ref(&instance_reports)));
    assert!(!validate(&[instance_reports.clone(), team_silent.clone()]));
    assert!(validate(&[instance_reports, team_silent, project_reports]));
}

#[test]
fn test_report_status_is_refused_for_task_without_reports_status_connection() {
    let silent = r#"{
        "forge": { "name": "forge", "owner": "instance", "git_host_type": "gitea", "created_by": "alice" }
    }"#;
    for task_fields in [
        r#""git_host_connection": "forge", "report_status": true"#,
        r#""report_status": true"#,
    ] {
        let cfg =
            make_state_with_git_host_connections_and_web_task_extra_fields(silent, task_fields);
        let errors = errors_of(&cfg);
        assert!(
            errors
                .iter()
                .any(|(field, _)| field == "tasks.web.report_status"),
            "{task_fields}: expected report_status error, got: {errors:?}"
        );
    }
}

#[test]
fn git_host_connection_owner_that_does_not_parse_fails_validation() {
    for owner in ["cache/x", "team/", "acme"] {
        let cfg = state_with_one_user_one_project_and_git_host_connections(&format!(
            r#"{{ "forge": {{ "name": "forge", "owner": "{owner}", "git_host_type": "gitea", "created_by": "alice" }} }}"#
        ));
        let errors = errors_of(&cfg);
        assert!(
            errors.iter().any(
                |(field, message)| field == "git_host_connections.forge.owner"
                    && message.contains(owner)
            ),
            "{owner}: expected owner parse error, got: {errors:?}"
        );
    }
}

#[test]
fn rejects_owner_naming_undeclared_team_or_project() {
    for owner in ["team/ghost", "project/ghost"] {
        let cfg = state_with_one_user_one_project_and_git_host_connections(&format!(
            r#"{{ "forge": {{ "name": "forge", "owner": "{owner}", "git_host_type": "gitea", "created_by": "alice" }} }}"#
        ));
        let errors = errors_of(&cfg);
        assert!(
            errors.iter().any(
                |(field, message)| field == "git_host_connections.forge.owner"
                    && message.contains("ghost")
            ),
            "{owner}: expected unknown owner error, got: {errors:?}"
        );
    }
}

#[test]
fn rejects_duplicate_connection_name_for_same_owner_but_allows_it_for_different_owners() {
    let duplicated = state_with_one_user_one_project_and_git_host_connections(
        r#"{
            "a": { "name": "forge", "owner": "project/acme", "git_host_type": "gitea", "created_by": "alice" },
            "b": { "name": "forge", "owner": "project/acme", "git_host_type": "gitlab", "created_by": "alice" }
        }"#,
    );
    let errors = errors_of(&duplicated);
    assert_eq!(
        errors
            .iter()
            .filter(|(_, message)| message.contains("duplicate"))
            .count(),
        1,
        "expected one duplicate error, got: {errors:?}"
    );

    let spread = state_with_one_user_one_project_and_git_host_connections(
        r#"{
            "a": { "name": "forge", "owner": "project/acme", "git_host_type": "gitea", "created_by": "alice" },
            "b": { "name": "forge", "owner": "instance", "git_host_type": "gitea", "created_by": "alice" }
        }"#,
    );
    let v = spread.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn state_github_connection_requires_installation_id() {
    let connections = r#"{
        "gh": { "name": "gh", "owner": "instance", "git_host_type": "github", "created_by": "alice" }
    }"#;
    let cfg = state_with_one_user_one_project_and_git_host_connections(connections);
    let errors = errors_of(&cfg);
    assert!(
        errors
            .iter()
            .any(|(field, _)| field == "git_host_connections.gh.installation_id"),
        "expected installation_id requirement, got: {errors:?}"
    );
}

#[test]
fn state_github_connection_with_installation_id_is_valid() {
    let connections = r#"{
        "gh": { "name": "gh", "owner": "instance", "git_host_type": "github", "installation_id": 42, "account_login": "acme", "created_by": "alice" }
    }"#;
    let cfg = state_with_one_user_one_project_and_git_host_connections(connections);
    let v = cfg.validate();
    assert!(v.is_valid, "errors: {:?}", v.errors);
}

#[test]
fn installation_id_on_connection_not_owned_by_instance_is_refused() {
    let connections = r#"{
        "gh": { "name": "gh", "owner": "project/acme", "git_host_type": "github", "installation_id": 42, "created_by": "alice" }
    }"#;
    let cfg = state_with_one_user_one_project_and_git_host_connections(connections);
    let errors = errors_of(&cfg);
    assert!(
        errors.iter().any(
            |(field, message)| field == "git_host_connections.gh.installation_id"
                && message.contains("instance")
        ),
        "expected instance-owner requirement, got: {errors:?}"
    );
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
                    { "name": "flake-update", "type": "open_pr", "config": { "git_host_connection": "gh" } }
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
                    { "name": "x", "type": "open_pr", "events": ["build.completed"], "config": { "git_host_connection": "gh" } }
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
    // Old state files may still set `force_evaluation`. Serde is dropping unknown fields by
    // default, and parsing must keep working.
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
fn state_worker_without_team_rejects_empty_projects() {
    let cfg = worker_cfg("[]");
    let v = cfg.validate();
    assert!(!v.is_valid);
    assert!(
        v.errors.iter().any(
            |e| e.field == "workers.550e8400-e29b-41d4-a716-446655440001.projects"
                && e.message
                    .contains("a team or be registered under at least one project")
        ),
        "expected at-least-one-project error, got: {:?}",
        v.errors
    );
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
fn state_worker_accepts_missing_created_by() {
    let json = r#"{
        "teams": { "server": { "name": "server", "display_name": "Server" } },
        "workers": {
            "local": {
                "worker_id": "550e8400-e29b-41d4-a716-446655440099",
                "token_file": "/dev/null",
                "display_name": "Local Worker",
                "team": "server"
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
        "teams": { "server": { "name": "server", "display_name": "Server" } },
        "workers": {
            "builder-1": {
                "worker_id": "550e8400-e29b-41d4-a716-446655440001",
                "token_file": "/dev/null",
                "display_name": "Builder",
                "team": "server",
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

#[test]
fn a_team_with_an_unknown_member_is_rejected() {
    let json = r#"{
        "teams": {
            "platform": {
                "name": "platform", "display_name": "Platform",
                "members": [{ "user": "ghost", "role": "Admin" }]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "teams.platform.members.ghost.user"),
        "{:?}",
        v.errors
    );
}

#[test]
fn a_project_grant_with_users_needs_a_role() {
    let json = r#"{
        "users": { "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" } },
        "teams": { "platform": { "name": "platform", "display_name": "Platform" } },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null",
                "public": false, "created_by": "alice",
                "teams": [{ "team": "platform", "users": true, "workers": false }]
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "projects.acme.teams.platform.role"),
        "{:?}",
        v.errors
    );
}

#[test]
fn an_unknown_owner_team_is_rejected() {
    let json = r#"{
        "users": { "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" } },
        "projects": {
            "acme": {
                "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null",
                "public": false, "created_by": "alice", "owner_team": "ghosts"
            }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "projects.acme.owner_team"),
        "{:?}",
        v.errors
    );
}

#[test]
fn a_worker_belongs_to_a_team_or_to_projects() {
    let json = r#"{
        "teams": { "platform": { "name": "platform", "display_name": "Platform" } },
        "workers": {
            "both": { "worker_id": "w1", "projects": ["acme"], "team": "platform", "token_file": "/dev/null", "display_name": "Both" },
            "neither": { "worker_id": "w2", "projects": [], "token_file": "/dev/null", "display_name": "Neither" }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.errors.iter().any(|e| e.field == "workers.w1.team"),
        "{:?}",
        v.errors
    );
    assert!(
        v.errors.iter().any(|e| e.field == "workers.w2.projects"),
        "{:?}",
        v.errors
    );
}

#[test]
fn a_worker_of_an_unknown_team_is_rejected() {
    let json = r#"{
        "workers": { "w": { "worker_id": "w3", "team": "ghosts", "token_file": "/dev/null", "display_name": "W" } }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.errors.iter().any(|e| e.field == "workers.w3.team"),
        "{:?}",
        v.errors
    );
}

#[test]
fn a_start_evaluation_action_survives_export() {
    use gradient_entity::ids::TaskId;
    use gradient_types::actions::{ActionConfig, ActionType};
    use std::collections::HashMap;

    let deploy = TaskId::now_v7();
    let action = gradient_entity::task_action::Model {
        name: "evaluate-deploy".into(),
        action_type: ActionType::StartEvaluation,
        config: serde_json::to_value(ActionConfig::StartEvaluation { task: deploy }).unwrap(),
        events: serde_json::json!(["exec_job.completed"]),
        active: true,
        ..Default::default()
    };
    let task_name = HashMap::from([(deploy, "deploy".to_string())]);

    let exported = crate::export::export_action(&action, &HashMap::new(), &task_name)
        .expect("the action is exported");

    assert_eq!(exported.action_type, "start_evaluation");
    assert_eq!(exported.config["task"], "deploy");
    assert_eq!(exported.events, vec!["exec_job.completed".to_string()]);
}

#[test]
fn rejects_project_role_with_server_wide_create_project() {
    let json = r#"{
        "users": {
            "alice": { "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }
        },
        "projects": {
            "acme": { "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice" }
        },
        "roles": {
            "creator": { "name": "creator", "project": "acme", "permissions": ["viewProject", "createProject"] }
        }
    }"#;
    let cfg: StateConfiguration = serde_json::from_str(json).unwrap();
    let v = cfg.validate();
    assert!(
        v.errors
            .iter()
            .any(|e| e.field == "roles.creator.permissions"),
        "errors: {:?}",
        v.errors
    );
}

#[test]
fn pinned_state_file_api_key_cannot_list_create_project_but_unpinned_can() {
    let state_with_key = |project: &str| {
        let json = format!(
            r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
            }},
            "projects": {{
                "acme": {{ "name": "acme", "display_name": "ACME", "private_key_file": "/dev/null", "public": false, "created_by": "alice" }}
            }},
            "api_keys": {{
                "ci": {{ "name": "ci", "key_file": "/dev/null", "owned_by": "alice", "permissions": ["createProject"]{project} }}
            }}
        }}"#
        );
        serde_json::from_str::<StateConfiguration>(&json)
            .unwrap()
            .validate()
    };

    let pinned = state_with_key(r#", "project": "acme""#);
    let unpinned = state_with_key("");

    assert!(
        pinned
            .errors
            .iter()
            .any(|e| e.field == "api_keys.ci.permissions"),
        "errors: {:?}",
        pinned.errors
    );
    assert!(unpinned.is_valid, "errors: {:?}", unpinned.errors);
}

#[test]
fn rejects_use_superuser_rights_on_api_key_of_non_superuser_but_allows_superuser() {
    let state_with_owner = |superuser: bool| {
        let json = format!(
            r#"{{
            "users": {{
                "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null", "superuser": {superuser} }}
            }},
            "api_keys": {{
                "admin": {{ "name": "admin", "key_file": "/dev/null", "owned_by": "alice", "permissions": ["useSuperuserRights"] }}
            }}
        }}"#
        );
        serde_json::from_str::<StateConfiguration>(&json)
            .unwrap()
            .validate()
    };

    let regular_owner = state_with_owner(false);
    let superuser_owner = state_with_owner(true);

    assert!(
        regular_owner
            .errors
            .iter()
            .any(|e| e.field == "api_keys.admin.permissions"),
        "errors: {:?}",
        regular_owner.errors
    );
    assert!(
        superuser_owner.is_valid,
        "errors: {:?}",
        superuser_owner.errors
    );
}

fn validate_state_with_alice_and_ci_api_key_json(
    api_key_json: &str,
) -> crate::validation::ValidationResult {
    let json = format!(
        r#"{{
        "users": {{
            "alice": {{ "username": "alice", "name": "Alice", "email": "a@x.io", "password_file": "/dev/null" }}
        }},
        "api_keys": {{
            "ci": {api_key_json}
        }}
    }}"#
    );
    serde_json::from_str::<StateConfiguration>(&json)
        .unwrap()
        .validate()
}

#[test]
fn state_api_key_with_unknown_cache_permission_is_rejected() {
    let unknown = validate_state_with_alice_and_ci_api_key_json(
        r#"{ "name": "ci", "key_file": "/dev/null", "owned_by": "alice", "cache_permissions": ["pushEverything"] }"#,
    );
    let cache_only = validate_state_with_alice_and_ci_api_key_json(
        r#"{ "name": "ci", "key_file": "/dev/null", "owned_by": "alice", "cache_permissions": ["readStore"] }"#,
    );

    assert!(
        unknown
            .errors
            .iter()
            .any(|e| e.field == "api_keys.ci.cache_permissions"),
        "errors: {:?}",
        unknown.errors
    );
    assert!(cache_only.is_valid, "errors: {:?}", cache_only.errors);
}

#[test]
fn state_validation_rejects_api_key_pinned_to_project_with_cache_permissions() {
    let result = validate_state_with_alice_and_ci_api_key_json(
        r#"{ "name": "ci", "key_file": "/dev/null", "owned_by": "alice", "project": "web", "permissions": ["viewProject"], "cache_permissions": ["readStore"] }"#,
    );

    assert!(
        result
            .errors
            .iter()
            .any(|e| e.field == "api_keys.ci.cache_permissions"),
        "errors: {:?}",
        result.errors
    );
}
