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

pub type FoundDerivations<'a> = &'a (dyn Fn(Vec<ResolvedDerivation>) + Send + Sync);

#[derive(Debug, Default)]
pub struct FlakeDiscovery {
    pub warnings: Vec<String>,
    pub errors: Vec<AttrError>,
}

#[async_trait]
pub trait ImportBuilder: Send + Sync {
    async fn build_imports(&self, derived_paths: Vec<String>) -> Result<(), String>;
}

pub struct RefuseImports(pub &'static str);

#[async_trait]
impl ImportBuilder for RefuseImports {
    async fn build_imports(&self, _derived_paths: Vec<String>) -> Result<(), String> {
        Err(format!(
            "import from derivation is not available during {}",
            self.0
        ))
    }
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
        imports: &dyn ImportBuilder,
        found: FoundDerivations<'_>,
    ) -> Result<FlakeDiscovery>;

    async fn release_evaluators(&self);

    async fn get_derivation(&self, drv_path: String) -> Result<Derivation>;

    async fn get_features(&self, drv_path: String) -> Result<(String, Vec<String>)>;
}
