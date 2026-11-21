/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::super::DynError;
use super::super::StateApplicator;
use super::super::{parse_password_phc, read_credential};
use crate::config::*;
use anyhow::Result;
use gradient_db::users::ValidatedNewUser;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use std::collections::HashMap;

impl<'a> StateApplicator<'a> {
    pub(crate) async fn apply_users(
        &self,
        state_users: &HashMap<String, StateUser>,
    ) -> Result<(), DynError> {
        for state_user in state_users.values() {
            // An unset `password_file` is storing `None` for an OIDC-only user. The OIDC login flow
            // is then accepting the account instead of rejecting it as a password account.
            let password_hash = if state_user.password_file.is_some() {
                let (contents, path) = read_credential(
                    self.credentials_dir,
                    "user",
                    &state_user.username,
                    "password",
                    "password file",
                )?;
                Some(parse_password_phc(&contents, &path)?)
            } else {
                None
            };

            let existing_account_user_id = match &state_user.existing_account_id {
                Some(raw) => Some(raw.trim().parse::<UserId>().map_err(|e| {
                    format!(
                        "Invalid existing_account_id '{}' for user '{}': {}",
                        raw, state_user.username, e
                    )
                })?),
                None => None,
            };
            let holder_of_username =
                gradient_db::users::by_username(self.db, &state_user.username).await?;
            let holder_of_email = gradient_db::users::by_email(self.db, &state_user.email).await?;
            let own_account = match existing_account_user_id {
                Some(id) => EUser::find_by_id(id).one(self.db).await?,
                None => holder_of_username
                    .clone()
                    .filter(|user| user.account_origin == AccountOrigin::DeclaredInStateFile),
            };
            let own_account_id = own_account.as_ref().map(|user| user.id);
            let conflict = [holder_of_username, holder_of_email]
                .into_iter()
                .flatten()
                .find(|holder| Some(holder.id) != own_account_id);
            if let Some(conflict) = conflict {
                tracing::error!(
                    username = %state_user.username,
                    conflicting_account_id = %conflict.id,
                    conflicting_account_origin = ?conflict.account_origin,
                    "Skip the state user. There is an account from another origin with the same username or email. Rename or delete that account. Or set existing_account_id on the declared user to link it to that account. That link deletes the password, sessions, API keys and external ID links of the account."
                );
                continue;
            }

            match own_account {
                Some(existing) => {
                    if existing.account_origin != AccountOrigin::DeclaredInStateFile {
                        gradient_db::users::revoke_previous_account_holder_access(
                            self.db,
                            existing.id,
                        )
                        .await?;
                        gradient_db::users::delete_account_identity_links_and_clear_scim_external_id(
                            self.db,
                            existing.id,
                        )
                        .await?;
                        tracing::warn!(
                            username = %state_user.username,
                            account_id = %existing.id,
                            previous_origin = ?existing.account_origin,
                            "A user on the state file took over an account from another origin. The cleanup deleted the password, sessions, API keys, external ID links and SCIM external ID of that account."
                        );
                    }
                    let mut user: AUser = existing.into();
                    user.account_origin = Set(AccountOrigin::DeclaredInStateFile);
                    user.username = Set(state_user.username.clone());
                    user.name = Set(state_user.name.clone());
                    user.email = Set(state_user.email.clone());
                    user.password = Set(password_hash);
                    user.email_verified = Set(state_user.email_verified);
                    user.superuser = Set(state_user.superuser);
                    user.managed = Set(true);
                    user.active = Set(true);
                    user.update(self.db).await?;
                    tracing::info!(username = %state_user.username, "Updated managed user");
                }
                None => {
                    let new_user = ValidatedNewUser::new(
                        AccountOrigin::DeclaredInStateFile,
                        &state_user.username,
                        &state_user.name,
                        Some(&state_user.email),
                    )
                    .map_err(|error| {
                        format!(
                            "User {} declared in the declarative state is invalid: {error}",
                            state_user.username
                        )
                    })?
                    .with_password_hash(password_hash)
                    .with_email_verified(state_user.email_verified)
                    .with_superuser_flag(state_user.superuser);
                    let new_user = match existing_account_user_id {
                        Some(id) => new_user.with_database_id(id),
                        None => new_user,
                    };
                    gradient_db::users::insert_user_account(self.db, new_user).await?;
                    tracing::info!(username = %state_user.username, "Created managed user");
                }
            }
        }

        Ok(())
    }
}
