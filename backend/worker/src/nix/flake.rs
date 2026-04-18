/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::executer::strip_nix_store_prefix;
use tracing::debug;

use crate::nix::nix_eval::{NixEvaluator, escape_nix_str};

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

/// Returns true when a wildcard pattern segment contains no wildcard characters.
fn is_literal_pattern(pattern: &str) -> bool {
    let path = pattern.strip_prefix('!').unwrap_or(pattern);
    !path.contains('*') && !path.contains('#')
}

/// Discovers all derivation attribute paths in a flake matching the given
/// wildcard patterns by evaluating the embedded `eval.nix` through the Nix C API.
///
/// Literal include patterns (no `*` or `#`) are returned directly **without**
/// calling `eval.nix`.  `eval.nix` checks `isDerivation` at each leaf by
/// forcing `v.type`, which can trigger deep NixOS module evaluation and
/// uncatchable `builtins.fetchGit` errors.  For an exact attr path the user
/// already knows the target; `get_derivation_path` will validate it by
/// evaluating `.drvPath` with proper per-attr error handling.
///
/// ## Wildcard semantics (for patterns passed to eval.nix)
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
    // Separate fully-literal include patterns from those that need eval.nix.
    let mut attrs: Vec<String> = wildcards
        .iter()
        .filter(|p| !p.starts_with('!') && is_literal_pattern(p))
        .cloned()
        .collect();

    let wildcard_patterns: Vec<String> = wildcards
        .iter()
        .filter(|p| !is_literal_pattern(p))
        .cloned()
        .collect();

    if !wildcard_patterns.is_empty() {
        let escaped_repo = escape_nix_str(repository);
        let wildcard_ref = build_wildcard_nix_expr(&wildcard_patterns);
        let expr = format!("({}) \"{}\" {}", EVAL_NIX, escaped_repo, wildcard_ref);

        debug!(wildcards = ?wildcard_patterns, "discovering derivations via eval.nix");

        let json_str = evaluator
            .eval_string(&expr)
            .context("eval.nix evaluation failed")?;

        let wildcard_attrs: Vec<String> =
            serde_json::from_str(&json_str).context("Failed to parse eval.nix JSON output")?;
        attrs.extend(wildcard_attrs);
    } else {
        debug!(wildcards = ?wildcards, "all patterns are literal; skipping eval.nix");
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_attr_path ───────────────────────────────────────────────────────

    #[test]
    fn split_attr_path_simple() {
        assert_eq!(
            split_attr_path("packages.x86_64-linux.hello"),
            vec!["packages", "x86_64-linux", "hello"]
        );
    }

    #[test]
    fn split_attr_path_quoted_dot() {
        let segs = split_attr_path(r#"packages."python3.12""#);
        assert_eq!(segs, vec!["packages", r#""python3.12""#]);
    }

    #[test]
    fn split_attr_path_wildcard() {
        assert_eq!(split_attr_path("*.*"), vec!["*", "*"]);
    }

    #[test]
    fn split_attr_path_single_segment() {
        assert_eq!(split_attr_path("packages"), vec!["packages"]);
    }

    // ── pattern_to_nix_list ───────────────────────────────────────────────────

    #[test]
    fn pattern_to_nix_list_simple() {
        assert_eq!(pattern_to_nix_list("a.b"), r#"[ "a" "b" ]"#);
    }

    #[test]
    fn pattern_to_nix_list_unquotes_inner() {
        // "python3.12" segment gets its outer quotes stripped, re-wrapped
        let result = pattern_to_nix_list(r#"packages."python3.12""#);
        assert_eq!(result, r#"[ "packages" "python3.12" ]"#);
    }

    #[test]
    fn pattern_to_nix_list_collapses_consecutive_wildcards() {
        // *.*  →  single "*"
        let result = pattern_to_nix_list("*.*");
        assert_eq!(result, r#"[ "*" ]"#);
    }

    #[test]
    fn pattern_to_nix_list_wildcard_then_name() {
        let result = pattern_to_nix_list("packages.*.hello");
        assert_eq!(result, r#"[ "packages" "*" "hello" ]"#);
    }

    // ── build_wildcard_nix_expr ───────────────────────────────────────────────

    #[test]
    fn build_wildcard_nix_expr_include_only() {
        let result = build_wildcard_nix_expr(&["packages.*.*".to_string()]);
        assert!(result.contains("\"include\""));
        assert!(result.contains("\"exclude\" = [  ]"));
    }

    #[test]
    fn build_wildcard_nix_expr_exclude_only() {
        let result = build_wildcard_nix_expr(&["!packages.x86_64-linux.broken".to_string()]);
        assert!(result.contains("\"include\" = [  ]"));
        assert!(result.contains("\"exclude\""));
    }

    #[test]
    fn build_wildcard_nix_expr_mixed() {
        let patterns = vec![
            "packages.*.*".to_string(),
            "!packages.x86_64-linux.broken".to_string(),
        ];
        let result = build_wildcard_nix_expr(&patterns);
        // Include should have the positive pattern
        assert!(result.contains("\"include\" = ["));
        // Exclude should have the negative pattern
        assert!(result.contains("\"exclude\" = ["));
        // The include should be non-empty and exclude should be non-empty
        let include_empty = result.contains("\"include\" = [  ]");
        let exclude_empty = result.contains("\"exclude\" = [  ]");
        assert!(!include_empty, "include should not be empty");
        assert!(!exclude_empty, "exclude should not be empty");
    }

    // ── build_wildcard_nix_expr multiple includes ─────────────────────────────

    /// Regression: comma-separated wildcards must be pre-split into individual
    /// patterns before being passed to `build_wildcard_nix_expr` / `discover_derivations`.
    /// Passing them as a single string like `"packages.x86_64-linux.*,checks.x86_64-linux.*"`
    /// would embed the literal comma into a Nix segment and find no derivations.
    #[test]
    fn build_wildcard_nix_expr_multiple_patterns_each_separate() {
        let patterns = vec![
            "packages.x86_64-linux.*".to_string(),
            "checks.x86_64-linux.*".to_string(),
        ];
        let result = build_wildcard_nix_expr(&patterns);
        // Both patterns must appear as separate Nix list entries
        assert_eq!(
            result,
            r#"{ "include" = [ [ "packages" "x86_64-linux" "*" ] [ "checks" "x86_64-linux" "*" ] ]; "exclude" = [  ]; }"#,
        );
    }

    #[test]
    fn build_wildcard_nix_expr_comma_in_single_string_is_wrong() {
        // This is the broken behaviour we fixed: the comma ends up inside a segment.
        let patterns = vec!["packages.x86_64-linux.*,checks.x86_64-linux.*".to_string()];
        let result = build_wildcard_nix_expr(&patterns);
        // The comma-containing segment makes the include list malformed —
        // it will NOT match the expected two-entry form.
        assert_ne!(
            result,
            r#"{ "include" = [ [ "packages" "x86_64-linux" "*" ] [ "checks" "x86_64-linux" "*" ] ]; "exclude" = [  ]; }"#,
        );
    }

    // ── is_literal_pattern / discover_derivations literal bypass ─────────────

    #[test]
    fn is_literal_pattern_plain() {
        assert!(is_literal_pattern(
            "nixosConfigurations.server1.config.system.build.toplevel"
        ));
        assert!(is_literal_pattern("packages.x86_64-linux.hello"));
    }

    #[test]
    fn is_literal_pattern_with_wildcard() {
        assert!(!is_literal_pattern("packages.*"));
        assert!(!is_literal_pattern("packages.#.hello"));
        assert!(!is_literal_pattern("*"));
    }

    #[test]
    fn is_literal_pattern_exclude_prefix() {
        // Exclude patterns with no wildcards are literal too.
        assert!(is_literal_pattern("!packages.x86_64-linux.broken"));
        assert!(!is_literal_pattern("!packages.*.broken"));
    }
}
