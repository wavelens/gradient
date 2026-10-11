/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::TeamUserId;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{team, team_user};

use crate::ids::{team_id, user_id};
use crate::test_date;

pub fn team() -> team::Model {
    team::Model {
        id: team_id(),
        name: "platform".into(),
        display_name: "Platform".into(),
        created_by: Some(user_id()),
        created_at: test_date(),
        ..Default::default()
    }
}

pub fn team_member(role: TeamRole) -> team_user::Model {
    team_user::Model {
        id: TeamUserId::now_v7(),
        team: team_id(),
        user: user_id(),
        role,
        ..Default::default()
    }
}
