/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Context;
use gradient_core::executer::strip_nix_store_prefix;

use super::nix_eval::{escape_nix_str, NixEvaluator};

/// Resolves a flake attribute path to its store derivation path using the
/// embedded Nix evaluator.
///
/// **Synchronous**: must run inside `tokio::task::spawn_blocking`.
pub(super) fn get_derivation_path(
    evaluator: &NixEvaluator,
    flake_ref: &str,
    attr_path: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    let expr = format!(
        "toString (builtins.getFlake \"{}\").{}.drvPath",
        escape_nix_str(flake_ref),
        attr_path,
    );

    let drv_path = evaluator
        .eval_string(&expr)
        .with_context(|| format!("nix eval drvPath failed for '{}#{}'", flake_ref, attr_path))?;

    Ok((strip_nix_store_prefix(&drv_path), vec![]))
}
