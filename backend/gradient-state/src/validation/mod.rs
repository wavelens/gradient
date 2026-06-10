/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pre-apply validation of a [`StateConfiguration`]. Each `State*` entity has
//! its own validator; [`StateConfiguration::validate`] runs them in order over a
//! shared [`EntityLookup`] / [`ErrorCollector`].

mod api_keys;
mod caches;
mod helpers;
mod integrations;
mod organizations;
mod projects;
mod roles;
mod users;
mod workers;

use crate::config::StateConfiguration;
use helpers::{EntityLookup, ErrorCollector};

#[derive(Debug, Clone, thiserror::Error)]
#[error("Validation error in field '{field}': {message}")]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub errors: Vec<ValidationError>,
    pub is_valid: bool,
}

impl StateConfiguration {
    pub fn validate(&self) -> ValidationResult {
        let lookup = EntityLookup::new(self);
        let mut errors = ErrorCollector::new();

        users::validate(&lookup, &mut errors);
        organizations::validate(&lookup, &mut errors);
        projects::validate(&lookup, &mut errors);
        integrations::validate(&lookup, &mut errors);
        caches::validate(&lookup, &mut errors);
        roles::validate(&lookup, &mut errors);
        api_keys::validate(&lookup, &mut errors);
        workers::validate(&lookup, &mut errors);

        let errors = errors.into_errors();
        ValidationResult {
            is_valid: errors.is_empty(),
            errors,
        }
    }
}
