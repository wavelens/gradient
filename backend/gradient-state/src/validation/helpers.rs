/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ValidationError;
use crate::config::StateConfiguration;
use gradient_types::input::InputError;
use std::collections::HashSet;

pub(super) struct ErrorCollector {
    errors: Vec<ValidationError>,
}

impl ErrorCollector {
    pub(super) fn new() -> Self {
        Self { errors: Vec::new() }
    }

    pub(super) fn push(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError {
            field: field.into(),
            message: message.into(),
        });
    }

    pub(super) fn record_input_error_at_field(
        &mut self,
        field: impl Into<String>,
        result: Result<(), InputError>,
    ) {
        if let Err(e) = result {
            self.push(field, e.to_string());
        }
    }

    pub(super) fn validate_optional_declared_uuid_and_record_duplicate_error(
        &mut self,
        field: String,
        kind: &str,
        id: Option<&str>,
        ids_seen: &mut HashSet<uuid::Uuid>,
    ) {
        let Some(id) = id else { return };
        match id.trim().parse::<uuid::Uuid>() {
            Ok(parsed) if !ids_seen.insert(parsed) => {
                self.push(field, format!("Duplicate {kind} id '{id}'"))
            }
            Ok(_) => {}
            Err(_) => self.push(field, format!("Invalid UUID '{id}'")),
        }
    }

    pub(super) fn into_errors(self) -> Vec<ValidationError> {
        self.errors
    }
}

pub(super) struct EntityLookup<'a> {
    pub(super) config: &'a StateConfiguration,
}

impl<'a> EntityLookup<'a> {
    pub(super) fn new(config: &'a StateConfiguration) -> Self {
        Self { config }
    }

    pub(super) fn user_exists(&self, name: &str) -> bool {
        self.config.users.contains_key(name)
    }

    /// Blank creator names leave the creator saved on the existing entity. Exports leave the creator blank when the creator is not an account in the state file.
    pub(super) fn creator_is_blank_or_user_exists(&self, name: &str) -> bool {
        name.is_empty() || self.user_exists(name)
    }

    pub(super) fn user_is_superuser(&self, name: &str) -> bool {
        self.config
            .users
            .get(name)
            .is_some_and(|user| user.superuser)
    }

    pub(super) fn project_exists(&self, name: &str) -> bool {
        self.config.projects.contains_key(name)
    }

    pub(super) fn team_exists(&self, name: &str) -> bool {
        self.config.teams.contains_key(name)
    }
}
