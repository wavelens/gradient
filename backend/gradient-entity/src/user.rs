/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::UserId;

#[derive(Clone, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: UserId,
    #[sea_orm(unique, indexed)]
    pub username: String,
    pub name: String,
    pub email: String,
    pub password: Option<String>,
    pub last_login_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
    pub email_verified: bool,
    pub email_verification_token: Option<String>,
    pub email_verification_token_expires: Option<NaiveDateTime>,
    pub managed: bool,
    pub superuser: bool,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
    pub active: bool,
    #[sea_orm(unique, indexed)]
    pub scim_external_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("username", &self.username)
            .field("name", &self.name)
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
