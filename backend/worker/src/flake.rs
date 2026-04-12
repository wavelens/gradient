/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::executer::strip_nix_store_prefix;
use tracing::debug;

use crate::nix_eval::{NixEvaluator, escape_nix_str};

/// The eval.nix expression embedded at compile time.
const EVAL_NIX: &str = include_str!("eval.nix");

/// Splits a Nix attribute path on `.`, respecting double-quoted segments.
/// `packages.x86_64-linux."python3.12".*` → `["packages", "x86_64-linux", "\"python3.12\"", "*"]`
fn split_attr_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in path.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '.' if !in_quotes => {
                segments.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    segments.push(current);
    segments
}

/// Converts a single dotted pattern (e.g. `packages.*."python3.12"`) into a
/// Nix list-of-strings literal: `[ "packages" "*" "python3.12" ]`.
///
/// Consecutive `"*"` segments are collapsed (mirrors `Wildcard::get_eval_str`).
fn pattern_to_nix_list(path: &str) -> String {
    let segs = split_attr_path(path);
    let raw_elems: Vec<String> = segs
        .into_iter()
        .map(|seg| {
            let content = if seg.starts_with('"') && seg.ends_with('"') && seg.len() >= 2 {
                seg[1..seg.len() - 1].to_string()
            } else {
                seg
            };
            format!("\"{}\"", content)
        })
        .collect();

    let mut elems: Vec<String> = Vec::new();
    for elem in raw_elems {
        if elem == "\"*\"" && elems.last().is_some_and(|l| l == "\"*\"") {
            continue;
        }
        elems.push(elem);
    }

    format!("[ {} ]", elems.join(" "))
}

/// Constructs a Nix `wildcard_ref` attrset expression from wildcard patterns.
fn build_wildcard_nix_expr(wildcards: &[String]) -> String {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();

    for pattern in wildcards {
        let (is_exclude, path) = match pattern.strip_prefix('!') {
            Some(body) => (true, body),
            None => (false, pattern.as_str()),
        };

        let nix_list = pattern_to_nix_list(path);
        if is_exclude {
            excludes.push(nix_list);
        } else {
            includes.push(nix_list);
        }
    }

    format!(
        "{{ \"include\" = [ {} ]; \"exclude\" = [ {} ]; }}",
        includes.join(" "),
        excludes.join(" "),
    )
}

/// Discovers all derivation attribute paths in a flake matching the given
/// wildcard patterns by evaluating the embedded `eval.nix` through the Nix C API.
///
/// ## Wildcard semantics
///
/// - `*` — **recursive** wildcard. Consecutive `*` segments are collapsed by
///   [`build_wildcard_nix_expr`] before being passed to `eval.nix`, so
///   `packages.*.*` and `packages.*` resolve identically. When `*` is the
///   last segment the evaluator also descends one additional level into nested
///   attrsets, which recovers the derivations that would have been found by the
///   collapsed trailing `.*`.
/// - `#` — **non-recursive** wildcard. Matches any attribute name at its
///   position but stops there; only nodes with `type == "derivation"` at
///   exactly that depth are collected. Use this when you want to target a
///   specific nesting level without the extra descent that `*` performs.
///
/// Synchronous — must run inside `spawn_blocking`.
pub(super) fn discover_derivations(
    evaluator: &NixEvaluator,
    repository: &str,
    wildcards: &[String],
) -> Result<Vec<String>> {
    let escaped_repo = escape_nix_str(repository);
    let wildcard_ref = build_wildcard_nix_expr(wildcards);
    let expr = format!(
        "({}) \"{}\" {}",
        EVAL_NIX, escaped_repo, wildcard_ref
    );

    debug!(wildcards = ?wildcards, "discovering derivations via eval.nix");

    let json_str = evaluator
        .eval_string(&expr)
        .context("eval.nix evaluation failed")?;

    let attrs: Vec<String> =
        serde_json::from_str(&json_str).context("Failed to parse eval.nix JSON output")?;

    Ok(attrs)
}

/// Resolves a flake attribute path to its store derivation path using the
/// embedded Nix evaluator.
///
/// **Synchronous**: must run inside `tokio::task::spawn_blocking`.
pub(super) fn get_derivation_path(
    evaluator: &NixEvaluator,
    flake_ref: &str,
    attr_path: &str,
) -> Result<(String, Vec<String>)> {
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
