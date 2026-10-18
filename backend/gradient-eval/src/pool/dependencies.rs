/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_trait::async_trait;

#[async_trait]
pub trait EvalDependencies: Send + Sync {
    async fn build_imports(&self, derived_paths: Vec<String>) -> Result<(), String>;

    async fn fetch_input(&self, request: String) -> Result<String, String>;
}

pub struct RefuseDependencies(pub &'static str);

#[async_trait]
impl EvalDependencies for RefuseDependencies {
    async fn build_imports(&self, _derived_paths: Vec<String>) -> Result<(), String> {
        Err(format!(
            "import from derivation is not available during {}",
            self.0
        ))
    }

    async fn fetch_input(&self, _request: String) -> Result<String, String> {
        Err(format!(
            "fetching inputs is not available during {}",
            self.0
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refusing_imports_names_the_caller() {
        let refused = RefuseDependencies("the local evaluation")
            .build_imports(vec!["/nix/store/aaaa-src.drv^out".into()])
            .await;

        assert_eq!(
            refused,
            Err("import from derivation is not available during the local evaluation".into())
        );
    }
}
