/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use gradient_core::db::Derivation;
use gradient_core::nix::{DerivationResolver, ResolvedDerivation};
use std::collections::HashMap;
use std::sync::Mutex;

/// In-memory `DerivationResolver` for unit tests.
///
/// Defaults are empty: `list_flake_derivations` returns `[]`, `resolve_derivation_paths`
/// resolves every attr to a deterministic placeholder drv path, `get_derivation` errors,
/// and `get_features` returns `(BUILTIN, [])`. Use the `with_*` builders to preload data.
#[derive(Debug, Default)]
pub struct FakeDerivationResolver {
    flake_attrs: Mutex<HashMap<String, Vec<String>>>,
    drv_paths: Mutex<HashMap<(String, String), String>>,
    derivations: Mutex<HashMap<String, Derivation>>,
    features: Mutex<HashMap<String, (String, Vec<String>)>>,
}

impl FakeDerivationResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_flake_attrs(self, flake: impl Into<String>, attrs: Vec<String>) -> Self {
        self.flake_attrs.lock().unwrap().insert(flake.into(), attrs);
        self
    }

    pub fn with_drv_path(
        self,
        flake: impl Into<String>,
        attr: impl Into<String>,
        drv_path: impl Into<String>,
    ) -> Self {
        self.drv_paths
            .lock()
            .unwrap()
            .insert((flake.into(), attr.into()), drv_path.into());
        self
    }

    pub fn with_derivation(self, drv_path: impl Into<String>, drv: Derivation) -> Self {
        self.derivations
            .lock()
            .unwrap()
            .insert(drv_path.into(), drv);
        self
    }

    pub fn with_features(
        self,
        drv_path: impl Into<String>,
        arch: impl Into<String>,
        features: Vec<String>,
    ) -> Self {
        self.features
            .lock()
            .unwrap()
            .insert(drv_path.into(), (arch.into(), features));
        self
    }
}

#[async_trait]
impl DerivationResolver for FakeDerivationResolver {
    async fn list_flake_derivations(
        &self,
        repository: String,
        _wildcards: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        Ok((
            self.flake_attrs
                .lock()
                .unwrap()
                .get(&repository)
                .cloned()
                .unwrap_or_default(),
            vec![],
        ))
    }

    async fn resolve_derivation_paths(
        &self,
        repository: String,
        attrs: Vec<String>,
    ) -> Result<(Vec<ResolvedDerivation>, Vec<String>)> {
        let drv_paths = self.drv_paths.lock().unwrap();
        Ok((
            attrs
                .into_iter()
                .map(|attr| {
                    let resolved = drv_paths
                        .get(&(repository.clone(), attr.clone()))
                        .cloned()
                        .map(|p| (p, vec![]))
                        .ok_or_else(|| anyhow!("no fake drv path for {}#{}", repository, attr));
                    (attr, resolved)
                })
                .collect(),
            vec![],
        ))
    }

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation> {
        self.derivations
            .lock()
            .unwrap()
            .get(&drv_path)
            .cloned()
            .ok_or_else(|| anyhow!("no fake derivation for {}", drv_path))
    }

    async fn get_features(&self, drv_path: String) -> Result<(String, Vec<String>)> {
        Ok(self
            .features
            .lock()
            .unwrap()
            .get(&drv_path)
            .cloned()
            .unwrap_or(("builtin".to_string(), vec![])))
    }
}
