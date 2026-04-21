/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod integrations;
pub mod management;
pub mod members;
pub mod settings;
pub mod ssh;
pub mod workers;

pub use self::integrations::*;
pub use self::management::*;
pub use self::members::*;
pub use self::settings::*;
pub use self::ssh::*;
pub use self::workers::*;

use crate::error::{WebError, WebResult};
use core::db::get_organization_by_name;
use core::types::{MOrganization, ServerState};
use std::sync::Arc;
use uuid::Uuid;

/// Load an organization that the given user is a member of.
///
/// Returns `not_found("Organization")` when the org doesn't exist or the user
/// is not a member, so callers cannot distinguish the two cases.
pub(super) async fn load_org_member(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
) -> WebResult<MOrganization> {
    get_organization_by_name(Arc::clone(state), user_id, org_name)
        .await?
        .ok_or_else(|| WebError::not_found("Organization"))
}

/// Load an organization that the user is a member of AND that is not
/// state-managed (i.e. editable via the API).
pub(super) async fn load_editable_org(
    state: &Arc<ServerState>,
    user_id: Uuid,
    org_name: String,
) -> WebResult<MOrganization> {
    let org = load_org_member(state, user_id, org_name).await?;
    if org.managed {
        return Err(WebError::Forbidden(
            "Cannot modify state-managed organization. This organization is managed by configuration and cannot be edited through the API.".to_string(),
        ));
    }
    Ok(org)
}
