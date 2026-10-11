/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{CacheId, CommitId, ProjectId, TaskId, TeamId, UserId};
use uuid::Uuid;

pub fn project_id() -> ProjectId {
    ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
}

pub fn user_id() -> UserId {
    UserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap())
}

pub fn task_id() -> TaskId {
    TaskId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap())
}

pub fn commit_id() -> CommitId {
    CommitId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000013").unwrap())
}

pub fn cache_id() -> CacheId {
    CacheId::new(Uuid::parse_str("d0000000-0000-0000-0000-000000000001").unwrap())
}

pub fn team_id() -> TeamId {
    TeamId::new(Uuid::parse_str("7e000000-0000-0000-0000-000000000001").unwrap())
}
