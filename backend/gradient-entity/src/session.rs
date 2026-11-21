/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::{NaiveDateTime, TimeDelta};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{SessionId, UserId};

#[derive(Clone, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "session")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: SessionId,
    pub user_id: UserId,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub last_used_at: NaiveDateTime,
    pub revoked_at: Option<NaiveDateTime>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub remember_me: bool,
    pub is_cli_device_sign_in_session: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Model {
    pub fn sign_in_session_valid_duration(remember_me: bool) -> TimeDelta {
        if remember_me {
            TimeDelta::days(30)
        } else {
            TimeDelta::hours(24)
        }
    }

    pub fn maximum_total_session_age(is_cli_device_sign_in_session: bool) -> TimeDelta {
        if is_cli_device_sign_in_session {
            TimeDelta::days(30)
        } else {
            TimeDelta::days(90)
        }
    }

    pub fn absolute_session_end_time(&self) -> NaiveDateTime {
        self.created_at + Self::maximum_total_session_age(self.is_cli_device_sign_in_session)
    }

    pub fn expiry_after_renewal_at(&self, at: NaiveDateTime) -> NaiveDateTime {
        (at + Self::sign_in_session_valid_duration(self.remember_me))
            .min(self.absolute_session_end_time())
            .max(self.expires_at)
    }

    pub fn is_usable_at(&self, now: NaiveDateTime) -> bool {
        self.revoked_at.is_none()
            && self.expires_at >= now
            && self.absolute_session_end_time() >= now
    }

    pub fn has_passed_half_validity_at(&self, now: NaiveDateTime) -> bool {
        self.expires_at < self.absolute_session_end_time()
            && self.expires_at - now < Self::sign_in_session_valid_duration(self.remember_me) / 2
    }
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("last_used_at", &self.last_used_at)
            .field("revoked_at", &self.revoked_at)
            .field("user_agent", &self.user_agent)
            .field("ip", &self.ip)
            .field("remember_me", &self.remember_me)
            .field(
                "is_cli_device_sign_in_session",
                &self.is_cli_device_sign_in_session,
            )
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_signed_in_ago(
        age: TimeDelta,
        is_cli_device_sign_in_session: bool,
    ) -> (Model, NaiveDateTime) {
        let now = chrono::Utc::now().naive_utc();
        let session = Model {
            created_at: now - age,
            expires_at: now + TimeDelta::hours(1),
            last_used_at: now,
            remember_me: true,
            is_cli_device_sign_in_session,
            ..Default::default()
        };
        (session, now)
    }

    #[test]
    fn browser_session_renewal_stops_at_day_90() {
        let (session, now) =
            session_signed_in_ago(TimeDelta::days(90) - TimeDelta::hours(1), false);
        assert_eq!(
            session.expiry_after_renewal_at(now),
            session.created_at + TimeDelta::days(90)
        );
    }

    #[test]
    fn cli_device_session_renewal_stops_at_day_30() {
        let (session, now) = session_signed_in_ago(TimeDelta::days(30) - TimeDelta::hours(1), true);
        assert_eq!(
            session.expiry_after_renewal_at(now),
            session.created_at + TimeDelta::days(30)
        );
    }

    #[test]
    fn renewal_never_moves_expiry_backwards() {
        let (mut session, now) = session_signed_in_ago(TimeDelta::days(1), false);
        session.expires_at = now + TimeDelta::days(40);
        assert_eq!(
            session.expiry_after_renewal_at(now),
            now + TimeDelta::days(40)
        );
    }

    #[test]
    fn session_whose_expiry_reached_absolute_end_is_not_due_for_renewal() {
        let (mut session, now) =
            session_signed_in_ago(TimeDelta::days(90) - TimeDelta::hours(1), false);
        session.expires_at = session.absolute_session_end_time();
        assert!(!session.has_passed_half_validity_at(now));
    }

    #[test]
    fn session_past_absolute_end_is_unusable_even_with_later_stored_expiry() {
        let (session, now) = session_signed_in_ago(TimeDelta::days(91), false);
        assert!(!session.is_usable_at(now));
    }
}
