/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::ValidationError;
use crate::config::StateConfiguration;

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

    pub(super) fn project_exists(&self, name: &str) -> bool {
        self.config.projects.contains_key(name)
    }
}
