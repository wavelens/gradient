/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure invite policy. No database, no HTTP: the rules that decide whether a
//! token may be redeemed, and how the two invite tables merge into one list.

use chrono::{Duration, NaiveDateTime};
use gradient_types::consts::INVITATION_VALIDITY_DAYS;
use gradient_types::ids::UserId;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InviteKind {
    Project,
    Cache,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteDecision {
    Redeem,
    Expired,
    NotInvitee,
}

/// Ownership is checked before expiry so an expired token never confirms to a
/// stranger that it was a real invitation.
pub fn evaluate_invite(
    invitee: UserId,
    caller: UserId,
    expires_at: NaiveDateTime,
    now: NaiveDateTime,
) -> InviteDecision {
    if invitee != caller {
        return InviteDecision::NotInvitee;
    }

    if now > expires_at {
        return InviteDecision::Expired;
    }

    InviteDecision::Redeem
}

pub fn invitation_expiry(now: NaiveDateTime) -> NaiveDateTime {
    now + Duration::days(INVITATION_VALIDITY_DAYS)
}

#[derive(Clone, Debug, Serialize)]
pub struct InviteItem {
    pub kind: InviteKind,
    pub token: String,
    pub scope: String,
    pub scope_display_name: String,
    pub role: String,
    pub invited_by: String,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
}

pub fn merge_invites(project: Vec<InviteItem>, cache: Vec<InviteItem>) -> Vec<InviteItem> {
    let mut all = project;
    all.extend(cache);
    all.sort_by_key(|i| std::cmp::Reverse(i.created_at));
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};
    use uuid::uuid;

    fn at(seconds: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 5)
            .expect("valid date")
            .and_hms_opt(12, 0, 0)
            .expect("valid time")
            + Duration::seconds(seconds)
    }

    fn invitee() -> UserId {
        UserId::new(uuid!("11111111-1111-1111-1111-111111111111"))
    }

    fn stranger() -> UserId {
        UserId::new(uuid!("22222222-2222-2222-2222-222222222222"))
    }

    #[test]
    fn invitee_redeems_before_expiry() {
        assert_eq!(
            evaluate_invite(invitee(), invitee(), at(60), at(0)),
            InviteDecision::Redeem
        );
    }

    #[test]
    fn expiry_is_inclusive_of_the_last_second() {
        assert_eq!(
            evaluate_invite(invitee(), invitee(), at(0), at(0)),
            InviteDecision::Redeem
        );
        assert_eq!(
            evaluate_invite(invitee(), invitee(), at(0), at(1)),
            InviteDecision::Expired
        );
    }

    #[test]
    fn a_forwarded_token_does_not_let_a_stranger_redeem() {
        assert_eq!(
            evaluate_invite(invitee(), stranger(), at(60), at(0)),
            InviteDecision::NotInvitee
        );
    }

    #[test]
    fn ownership_is_checked_before_expiry() {
        assert_eq!(
            evaluate_invite(invitee(), stranger(), at(0), at(60)),
            InviteDecision::NotInvitee
        );
    }

    #[test]
    fn expiry_is_seven_days_out() {
        assert_eq!(invitation_expiry(at(0)), at(0) + Duration::days(7));
    }

    fn item(kind: InviteKind, scope: &str, created: i64) -> InviteItem {
        InviteItem {
            kind,
            token: format!("token-{scope}"),
            scope: scope.to_string(),
            scope_display_name: scope.to_string(),
            role: "Admin".to_string(),
            invited_by: "alice".to_string(),
            created_at: at(created),
            expires_at: at(created + 100),
        }
    }

    #[test]
    fn merge_returns_both_kinds_newest_first() {
        let merged = merge_invites(
            vec![item(InviteKind::Project, "older-project", 0)],
            vec![item(InviteKind::Cache, "newer-cache", 10)],
        );

        let scopes: Vec<&str> = merged.iter().map(|i| i.scope.as_str()).collect();
        assert_eq!(scopes, vec!["newer-cache", "older-project"]);
        assert_eq!(merged[0].kind, InviteKind::Cache);
    }

    #[test]
    fn merge_handles_one_empty_side() {
        let merged = merge_invites(vec![], vec![item(InviteKind::Cache, "only", 0)]);
        assert_eq!(merged.len(), 1);
    }
}
