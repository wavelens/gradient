/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{
    EvaluationId, ProjectId, ProjectUserId, RoleId, TaskId, UploadSessionId,
};
use gradient_entity::{evaluation, project, project_user, role, task, upload_session};
use gradient_types::ConcurrencyPolicy;
use gradient_types::consts::{BASE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID, BASE_ROLE_WRITE_ID};
use gradient_types::permissions::{PermissionMask, admin_mask, view_mask, write_mask};

use crate::ids::{commit_id, project_id, task_id, user_id};
use crate::test_date;

pub fn project() -> project::Model {
    project::Model {
        id: project_id(),
        name: "test-project".into(),
        display_name: "Test Project".into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

pub fn public_project() -> project::Model {
    project::Model {
        public: true,
        ..project()
    }
}

pub fn project_with_id(id: ProjectId, slug: &str) -> project::Model {
    project::Model {
        id,
        name: slug.into(),
        display_name: slug.into(),
        public_key: "ssh-ed25519 AAAA test".into(),
        private_key: "encrypted".into(),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

pub fn task() -> task::Model {
    task::Model {
        id: task_id(),
        project: project_id(),
        name: "test-task".into(),
        active: true,
        display_name: "Test Task".into(),
        repository: "https://github.com/test/repo".into(),
        wildcard: "*".into(),
        last_check_at: test_date(),
        created_by: user_id(),
        created_at: test_date(),
        keep_evaluations: 10,
        concurrency: ConcurrencyPolicy::Skip,
        sign_cache: true,
        ..Default::default()
    }
}

pub fn task_without_cache_signing() -> task::Model {
    task::Model {
        sign_cache: false,
        ..task()
    }
}

pub fn admin_role() -> role::Model {
    builtin_role(BASE_ROLE_ADMIN_ID, "Admin", admin_mask())
}

pub fn write_role() -> role::Model {
    builtin_role(BASE_ROLE_WRITE_ID, "Write", write_mask())
}

pub fn view_role() -> role::Model {
    builtin_role(BASE_ROLE_VIEW_ID, "View", view_mask())
}

fn builtin_role(id: RoleId, name: &str, permission: PermissionMask) -> role::Model {
    role::Model {
        id,
        name: name.into(),
        permission,
        ..Default::default()
    }
}

pub fn project_member(role: RoleId) -> project_user::Model {
    project_user::Model {
        id: ProjectUserId::now_v7(),
        project: project_id(),
        user: user_id(),
        role,
    }
}

pub fn eval_at(id: EvaluationId, offset_secs: i64) -> evaluation::Model {
    let created_at = test_date() + chrono::Duration::seconds(offset_secs);
    evaluation::Model {
        id,
        task: Some(task_id()),
        repository: "https://github.com/test/repo".into(),
        commit: commit_id(),
        wildcard: "*".into(),
        status: evaluation::EvaluationStatus::Completed,
        created_at,
        updated_at: created_at,
        ..Default::default()
    }
}

pub fn project_named(name: &str) -> project::Model {
    project::Model {
        id: ProjectId::now_v7(),
        name: name.to_string(),
        display_name: format!("{} display", name),
        created_by: user_id(),
        created_at: test_date(),
        ..Default::default()
    }
}

pub fn upload_session(
    id: UploadSessionId,
    missing: Vec<String>,
    dispatched: bool,
    expired: bool,
) -> upload_session::Model {
    let now = chrono::Utc::now().naive_utc();
    let expires_at = if expired {
        now - chrono::Duration::seconds(60)
    } else {
        now + chrono::Duration::hours(1)
    };
    upload_session::Model {
        id,
        project: project_id(),
        manifest: serde_json::json!([]),
        missing: serde_json::to_value(missing).unwrap(),
        created_at: now,
        expires_at,
        dispatched_at: if dispatched { Some(now) } else { None },
        ..Default::default()
    }
}

pub fn build_request_task(id: TaskId) -> task::Model {
    task::Model {
        id,
        project: project_id(),
        name: "build-request".into(),
        active: true,
        display_name: "Build Requests".into(),
        description: "Server-managed task for `gradient build` submissions.".into(),
        repository: "build-request".into(),
        wildcard: "*".into(),
        last_check_at: chrono::NaiveDateTime::default(),
        created_by: user_id(),
        created_at: chrono::Utc::now().naive_utc(),
        managed: true,
        keep_evaluations: 30,
        concurrency: ConcurrencyPolicy::SoftAbort,
        sign_cache: true,
        ..Default::default()
    }
}
