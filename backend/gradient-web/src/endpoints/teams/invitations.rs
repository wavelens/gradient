/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::members::role_label;
use crate::audit::Audit;
use crate::endpoints::membership::find_user_by_username;
use crate::endpoints::membership::invitations::{PendingInvitationItem, RevokeInvitationRequest};
use crate::error::WebResult;
use crate::helpers::ok_json;
use crate::refusal::SharedRefusal;
use crate::scope::{Admin, TeamScope};
use axum::Json;
use gradient_types::events::EventOwner;
use gradient_types::events::audit::Action;
use gradient_types::*;

pub async fn get_team_invitations(
    TeamScope { team, db, .. }: TeamScope<Admin>,
) -> WebResult<Json<BaseResponse<Vec<PendingInvitationItem>>>> {
    let rows = gradient_db::teams::invitations::of_team(&db.web_db, team.id).await?;

    let items: Vec<PendingInvitationItem> = rows
        .iter()
        .map(|(inv, invitee)| {
            PendingInvitationItem::new(
                inv.user,
                invitee.as_ref(),
                role_label(inv.role).to_string(),
                inv.created_at,
                inv.expires_at,
            )
        })
        .collect();

    Ok(ok_json(items))
}

pub async fn delete_team_invitation(
    TeamScope { team, db, .. }: TeamScope<Admin>,
    audit: Audit,
    Json(body): Json<RevokeInvitationRequest>,
) -> WebResult<Json<BaseResponse<String>>> {
    let target_user = find_user_by_username(&db.web_db, &body.user).await?;

    let invitation =
        gradient_db::teams::invitations::by_team_and_user(&db.web_db, team.id, target_user.id)
            .await?
            .ok_or(SharedRefusal::NoPendingInvitation)?;

    gradient_db::teams::invitations::delete(&db.web_db, invitation).await?;

    audit.write(Action::TeamInvitationRevoke, EventOwner::default(), serde_json::json!({ "team_id": team.id.to_string(), "target_user_id": target_user.id.to_string(), }))
    .await;

    Ok(ok_json("Invitation revoked".to_string()))
}
