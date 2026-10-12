/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_trait::async_trait;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refusing_imports_names_the_caller() {
        let refused = RefuseImports("the local evaluation")
            .build_imports(vec!["/nix/store/aaaa-src.drv^out".into()])
            .await;

        assert_eq!(
            refused,
            Err("import from derivation is not available during the local evaluation".into())
        );
    }
}
