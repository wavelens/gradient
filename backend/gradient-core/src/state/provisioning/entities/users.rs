/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{parse_password_phc, read_credential};
use crate::state::config::*;
use crate::types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    // ── apply_users ───────────────────────────────────────────────────────────

    pub(crate) async fn apply_users(
        &self,
        state_users: &HashMap<String, StateUser>,
    ) -> Result<(), DynError> {
        for state_user in state_users.values() {
            // When password_file is set in the state config, a matching
            // systemd credential is loaded under GRADIENT_CREDENTIALS_DIR.
            // When unset (OIDC-only user), we store `None` so the OIDC
            // login flow in `gradient_web::authorization::oidc` will accept the
            // account instead of rejecting with "already exists with
            // password authentication".
            let password_hash = if state_user.password_file.is_some() {
                let (contents, path) =
                    read_credential("user", &state_user.username, "password", "password file")?;
                Some(parse_password_phc(&contents, &path)?)
            } else {
                None
            };

            let existing_user = user::Entity::find()
                .filter(user::Column::Username.eq(&state_user.username))
                .one(self.db)
                .await?;

            let now = now();

            if let Some(existing) = existing_user {
                let mut user: user::ActiveModel = existing.into();
                user.name = Set(state_user.name.clone());
                user.email = Set(state_user.email.clone());
                user.password = Set(password_hash.clone());
                user.email_verified = Set(state_user.email_verified);
                user.superuser = Set(state_user.superuser);
                user.managed = Set(true);
                user.update(self.db).await?;
                tracing::info!(username = %state_user.username, "Updated managed user");
            } else {
                let user = user::ActiveModel {
                    id: Set(UserId::now_v7()),
                    username: Set(state_user.username.clone()),
                    name: Set(state_user.name.clone()),
                    email: Set(state_user.email.clone()),
                    password: Set(password_hash),
                    last_login_at: Set(now),
                    created_at: Set(now),
                    email_verified: Set(state_user.email_verified),
                    email_verification_token: Set(None),
                    email_verification_token_expires: Set(None),
                    managed: Set(true),
                    superuser: Set(state_user.superuser),
                    oidc_issuer: Set(None),
                    oidc_subject: Set(None),
                };
                user.insert(self.db).await?;
                tracing::info!(username = %state_user.username, "Created managed user");
            }
        }

        Ok(())
    }
}
