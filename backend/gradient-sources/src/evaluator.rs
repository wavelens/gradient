/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use async_trait::async_trait;
use gradient_derivation::Derivation;
pub type ResolvedDerivation = (String, Result<(String, Vec<String>)>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AttrError {
    pub attr: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct FlakeDiscovery {
    pub derivations: Vec<ResolvedDerivation>,
    pub warnings: Vec<String>,
    pub errors: Vec<AttrError>,
}

/// Production impls must run inside `tokio::task::spawn_blocking`. The embedded Nix C API with
/// Boehm GC cannot run on signal-blocked Tokio workers.
#[async_trait]
pub trait DerivationResolver: Send + Sync + std::fmt::Debug + 'static {
    async fn list_flake_derivations(
        &self,
        repository: String,
        wildcards: Vec<String>,
        overrides: &[(String, String)],
    ) -> Result<FlakeDiscovery>;

    async fn release_evaluators(&self);

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation>;

    async fn get_features(&self, drv_path: String) -> Result<(String, Vec<String>)>;
}
