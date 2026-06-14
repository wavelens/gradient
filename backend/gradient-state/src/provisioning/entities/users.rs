/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{parse_password_phc, read_credential};
use crate::config::*;
use gradient_types::*;
use anyhow::Result;
use gradient_entity::*;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
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
                let user = user::Model {
                    id: UserId::now_v7(),
                    username: state_user.username.clone(),
                    name: state_user.name.clone(),
                    email: state_user.email.clone(),
                    password: password_hash,
                    last_login_at: now,
                    created_at: now,
                    email_verified: state_user.email_verified,
                    managed: true,
                    superuser: state_user.superuser,
                    active: true,
                    ..Default::default()
                }
                .into_active_model();

                user.insert(self.db).await?;
                tracing::info!(username = %state_user.username, "Created managed user");
            }
        }

        Ok(())
    }
}
