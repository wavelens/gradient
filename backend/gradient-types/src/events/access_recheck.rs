/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::audit::Action;
use super::{Event, EventRx};
use crate::constants::OPEN_CONNECTION_ACCESS_RECHECK_INTERVAL;
use crate::ids::UserId;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::time::{Instant, interval_at};

const AUDIT_ACTIONS_ON_OWN_ACCOUNT_THAT_CHANGE_ACCESS: &[Action] = &[
    Action::Logout,
    Action::UserDelete,
    Action::UserUpdateAccountSettings,
    Action::ApiKeyUpdate,
    Action::ApiKeyRevoke,
    Action::ApiKeyRegenerateSecret,
    Action::ApiKeyDelete,
    Action::SessionRevoke,
];

const AUDIT_ACTIONS_THAT_CHANGE_SHARED_RESOURCE_ACCESS: &[Action] = &[
    Action::ProjectUpdate,
    Action::ProjectDelete,
    Action::ProjectMemberRemove,
    Action::ProjectMemberRoleChange,
    Action::ProjectRoleUpdate,
    Action::ProjectRoleDelete,
    Action::TaskDelete,
    Action::TaskMoveToProject,
    Action::CacheUpdate,
    Action::CacheDelete,
    Action::CacheRoleUpdate,
    Action::CacheRoleDelete,
    Action::CacheMemberUpdate,
    Action::CacheMemberDelete,
    Action::CacheSubscriptionCancel,
    Action::TeamDelete,
    Action::TeamMemberRemove,
    Action::TeamMemberRoleChange,
    Action::TeamGrantUpdate,
    Action::TeamGrantRemove,
];

/// Access checks happen every 60 seconds and right after an audit event that can change the access
/// of the caller. No-access results end the wait at the first result. Events for shared resources
/// such as projects and caches recheck every caller. Events for a user account recheck only callers
/// signed in as that user. Those events never recheck a caller without an account (None).
pub async fn wait_until_access_check_fails<AccessCheck>(
    mut audit_events: EventRx,
    caller: Option<UserId>,
    mut access_still_holds: impl FnMut() -> AccessCheck,
) where
    AccessCheck: Future<Output = bool>,
{
    let period = OPEN_CONNECTION_ACCESS_RECHECK_INTERVAL;
    let mut recheck = interval_at(Instant::now() + period, period);
    let mut audit_events_open = true;
    loop {
        let received = tokio::select! {
            _ = recheck.tick() => None,
            event = audit_events.recv(), if audit_events_open => Some(event),
        };
        match received {
            None | Some(Err(RecvError::Lagged(_))) => {}
            Some(Ok(envelope)) if audit_event_can_change_stream_access(&envelope.event, caller) => {
            }
            Some(Ok(_)) => continue,
            Some(Err(RecvError::Closed)) => {
                audit_events_open = false;
                continue;
            }
        }
        audit_events_open &= drain_pending_audit_events(&mut audit_events);
        if !access_still_holds().await {
            return;
        }
    }
}

fn audit_event_can_change_stream_access(event: &Event, caller: Option<UserId>) -> bool {
    let Event::Audit(audited) = event else {
        return false;
    };
    if AUDIT_ACTIONS_THAT_CHANGE_SHARED_RESOURCE_ACCESS.contains(&audited.action) {
        return true;
    }
    AUDIT_ACTIONS_ON_OWN_ACCOUNT_THAT_CHANGE_ACCESS.contains(&audited.action)
        && caller.is_some_and(|caller| audited.user.is_none_or(|account| account == caller))
}

fn drain_pending_audit_events(audit_events: &mut EventRx) -> bool {
    loop {
        match audit_events.try_recv() {
            Ok(_) | Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty) => return true,
            Err(TryRecvError::Closed) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBus;
    use crate::events::audit::Audited;
    use std::time::Duration;
    use tokio::time::timeout;

    fn api_key_revoked_by(user: UserId) -> Audited {
        Audited {
            action: Action::ApiKeyRevoke,
            user: Some(user),
            owner: Default::default(),
            metadata: None,
        }
    }

    #[tokio::test]
    async fn revoking_own_api_key_audit_event_triggers_access_check_failure_but_revoking_other_user_api_key_does_not()
     {
        let bus = EventBus::default();
        let caller = UserId::now_v7();
        let wait = wait_until_access_check_fails(bus.subscribe(), Some(caller), || async { false });
        tokio::pin!(wait);

        bus.publish(api_key_revoked_by(UserId::now_v7()));
        assert!(timeout(Duration::from_millis(50), &mut wait).await.is_err());

        bus.publish(api_key_revoked_by(caller));
        assert!(timeout(Duration::from_secs(5), &mut wait).await.is_ok());
    }

    #[tokio::test]
    async fn test_caller_without_user_account_ignores_other_user_api_key_revoke_but_rechecks_on_project_update()
     {
        let bus = EventBus::default();
        let wait = wait_until_access_check_fails(bus.subscribe(), None, || async { false });
        tokio::pin!(wait);

        bus.publish(api_key_revoked_by(UserId::now_v7()));
        assert!(timeout(Duration::from_millis(50), &mut wait).await.is_err());

        bus.publish(Audited {
            action: Action::ProjectUpdate,
            ..api_key_revoked_by(UserId::now_v7())
        });
        assert!(timeout(Duration::from_secs(5), &mut wait).await.is_ok());
    }
}
