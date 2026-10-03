/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The action names are the strings stored in `audit_log.event`. Existing names must never
//! change.

use super::{EventKind, EventOwner};
use crate::ids::UserId;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

macro_rules! actions {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum Action {
            $($variant),*
        }

        impl Action {
            pub const ALL: &'static [Action] = &[$(Action::$variant),*];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Action::$variant => $name),*
                }
            }
        }
    };
}

actions! {
    LoginSuccess => "login.success",
    LoginFailure => "login.failure",
    Logout => "logout",
    Register => "register",
    UserDelete => "user.delete",
    ApiKeyCreate => "api_key.create",
    ApiKeyUpdate => "api_key.update",
    ApiKeyRevoke => "api_key.revoke",
    ApiKeyDelete => "api_key.delete",
    SshKeyCreate => "ssh_key.create",
    SshKeyDelete => "ssh_key.delete",
    SessionRevoke => "session.revoke",
    AuthDeny => "auth.deny",
    CliDeviceStart => "cli.device.start",
    CliDeviceAuthorize => "cli.device.authorize",
    CliDeviceDeny => "cli.device.deny",
    ProjectCreate => "project.create",
    ProjectUpdate => "project.update",
    ProjectDelete => "project.delete",
    ProjectStar => "project.star",
    ProjectUnstar => "project.unstar",
    ProjectMemberAdd => "project.member.add",
    ProjectMemberRemove => "project.member.remove",
    ProjectMemberRoleChange => "project.member.role_change",
    ProjectInvitationCreate => "project.invitation.create",
    ProjectInvitationRevoke => "project.invitation.revoke",
    ProjectInvitationAccept => "project.invitation.accept",
    ProjectInvitationDecline => "project.invitation.decline",
    ProjectRoleCreate => "project.role.create",
    ProjectRoleUpdate => "project.role.update",
    ProjectRoleDelete => "project.role.delete",
    ProjectWebhookCreate => "project.webhook.create",
    ProjectWebhookUpdate => "project.webhook.update",
    ProjectWebhookDelete => "project.webhook.delete",
    TaskCreate => "task.create",
    TaskUpdate => "task.update",
    TaskDelete => "task.delete",
    TaskStar => "task.star",
    TaskUnstar => "task.unstar",
    TaskActionCreate => "task.action.create",
    TaskActionUpdate => "task.action.update",
    TaskActionDelete => "task.action.delete",
    CacheCreate => "cache.create",
    CacheUpdate => "cache.update",
    CacheDelete => "cache.delete",
    CacheStar => "cache.star",
    CacheUnstar => "cache.unstar",
    CacheNarDelete => "cache.nar.delete",
    CacheNarUpload => "cache.nar.upload",
    CacheRoleCreate => "cache.role.create",
    CacheRoleUpdate => "cache.role.update",
    CacheRoleDelete => "cache.role.delete",
    CacheMemberCreate => "cache.member.create",
    CacheMemberUpdate => "cache.member.update",
    CacheMemberDelete => "cache.member.delete",
    CacheInvitationCreate => "cache.invitation.create",
    CacheInvitationRevoke => "cache.invitation.revoke",
    CacheInvitationAccept => "cache.invitation.accept",
    CacheInvitationDecline => "cache.invitation.decline",
    CacheSubscriptionRequest => "cache.subscription.request",
    CacheSubscriptionApprove => "cache.subscription.approve",
    CacheSubscriptionDeny => "cache.subscription.deny",
    CacheSubscriptionCancel => "cache.subscription.cancel",
    CacheWebhookCreate => "cache.webhook.create",
    CacheWebhookUpdate => "cache.webhook.update",
    CacheWebhookDelete => "cache.webhook.delete",
    InstanceWebhookCreate => "instance.webhook.create",
    InstanceWebhookUpdate => "instance.webhook.update",
    InstanceWebhookDelete => "instance.webhook.delete",
    TeamCreate => "team.create",
    TeamUpdate => "team.update",
    TeamDelete => "team.delete",
    TeamMemberAdd => "team.member.add",
    TeamMemberRemove => "team.member.remove",
    TeamMemberRoleChange => "team.member.role_change",
    TeamInvitationCreate => "team.invitation.create",
    TeamInvitationRevoke => "team.invitation.revoke",
    TeamInvitationAccept => "team.invitation.accept",
    TeamInvitationDecline => "team.invitation.decline",
    TeamGrantCreate => "team.grant.create",
    TeamGrantUpdate => "team.grant.update",
    TeamGrantRemove => "team.grant.remove",
    TeamRequestCreate => "team.request.create",
    TeamRequestApprove => "team.request.approve",
    TeamRequestDeny => "team.request.deny",
    TeamWorkerCreate => "team.worker.create",
    TeamWorkerUpdate => "team.worker.update",
    TeamWorkerDelete => "team.worker.delete",
}

impl Action {
    pub const fn personal(self) -> bool {
        use Action::*;
        matches!(
            self,
            LoginSuccess
                | LoginFailure
                | Logout
                | Register
                | UserDelete
                | ApiKeyCreate
                | ApiKeyUpdate
                | ApiKeyRevoke
                | ApiKeyDelete
                | SshKeyCreate
                | SshKeyDelete
                | SessionRevoke
                | AuthDeny
                | CliDeviceStart
                | CliDeviceAuthorize
                | CliDeviceDeny
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Audited {
    pub action: Action,
    pub user: Option<UserId>,
    #[serde(flatten)]
    pub owner: EventOwner,
    pub metadata: Option<serde_json::Value>,
}

const NAMES: [&str; Action::ALL.len()] = {
    let mut names = [""; Action::ALL.len()];
    let mut i = 0;
    while i < Action::ALL.len() {
        names[i] = Action::ALL[i].name();
        i += 1;
    }
    names
};

impl EventKind for Audited {
    const NAME: &'static str = "audit.<action>";
    const DURABLE: bool = true;
    const NAMES: &'static [&'static str] = &NAMES;

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(self.action.name())
    }

    fn owner(&self) -> EventOwner {
        self.owner
    }

    fn personal(&self) -> bool {
        self.action.personal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn audit_names_are_unique() {
        let names: HashSet<_> = Action::ALL.iter().map(|a| a.name()).collect();
        assert_eq!(names.len(), Action::ALL.len());
    }
}
