/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure wildcard-pattern traversal over a flake's output attr tree.
//!
//! Reproduces the segment semantics of the retired `eval.nix` resolver:
//! `*` (one level; trailing `*` recovers the collapsed second level, but stops
//! at opaque typed attrsets), `#` (recurses on non-last, derivations at one
//! depth when trailing), literal (exact), exclusions (exact-path), and
//! consecutive-`*` collapse. Abstracted over [`WalkNode`] so it unit-tests with
//! a stub tree.

use anyhow::Result;

/// A node in the flake-output attr tree (a real impl wraps an eval-cache AttrCursor).
pub(crate) trait WalkNode: Sized {
    /// Child attribute names (sorted).
    fn child_names(&self) -> Result<Vec<String>>;
    /// Child node by name, or `None` if absent.
    fn child(&self, name: &str) -> Result<Option<Self>>;
    /// Whether this node is a derivation.
    fn is_derivation(&self) -> Result<bool>;
    /// Whether this node is an opaque typed attrset (e.g. a NixOS option) that
    /// is not a derivation — `*` traversal must not descend into it.
    fn is_opaque(&self) -> Result<bool>;
}

/// Drop a `*` segment immediately following another `*` (`packages.*.*` == `packages.*`).
pub(crate) fn collapse_stars(segs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in segs {
        if s == "*" && out.last().map(|p| p == "*").unwrap_or(false) {
            continue;
        }

        out.push(s.clone());
    }

    out
}

/// Parse one wildcard string into (is_exclude, segments). Mirrors the worker's
/// pattern format: `.`-separated segments, optional leading `!` = exclude.
/// Double-quoted spans keep an inner `.` within one segment (the quotes are
/// stripped), e.g. `pkgs."python3.12".*` → `["pkgs", "python3.12", "*"]`.
pub(crate) fn parse_pattern(pat: &str) -> (bool, Vec<String>) {
    let (exclude, body) = match pat.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, pat),
    };

    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in body.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            '.' if !in_quotes => {
                segs.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }

    segs.push(cur);

    (exclude, segs)
}

/// Walk `node` matching `segs`, pushing full dotted attr paths of matched
/// derivations into `out`. `path` is the accumulated path to `node`.
fn walk<N: WalkNode>(node: &N, path: &[String], segs: &[String], out: &mut Vec<String>) -> Result<()> {
    match segs.split_first() {
        None => {
            if node.is_derivation()? {
                out.push(path.join("."));
            }
        }
        Some((seg, rest)) if seg == "*" => {
            for name in node.child_names()? {
                let Some(child) = node.child(&name)? else {
                    continue;
                };
                let mut p = path.to_vec();
                p.push(name);

                if rest.is_empty() {
                    if child.is_derivation()? {
                        out.push(p.join("."));
                    } else if child.is_opaque()? {
                        continue;
                    } else {
                        for sub in child.child_names()? {
                            let Some(gc) = child.child(&sub)? else {
                                continue;
                            };

                            if gc.is_derivation()? {
                                let mut q = p.clone();
                                q.push(sub);
                                out.push(q.join("."));
                            }
                        }
                    }
                } else if child.is_opaque()? {
                    continue;
                } else {
                    walk(&child, &p, rest, out)?;
                }
            }
        }
        Some((seg, rest)) if seg == "#" => {
            for name in node.child_names()? {
                let Some(child) = node.child(&name)? else {
                    continue;
                };
                let mut p = path.to_vec();
                p.push(name);

                if rest.is_empty() {
                    if child.is_derivation()? {
                        out.push(p.join("."));
                    }
                } else {
                    walk(&child, &p, rest, out)?;
                }
            }
        }
        Some((seg, rest)) => {
            if let Some(child) = node.child(seg)? {
                let mut p = path.to_vec();
                p.push(seg.clone());
                walk(&child, &p, rest, out)?;
            }
        }
    }

    Ok(())
}

/// Discover all derivation attr paths matching `includes`, minus `excludes`
/// (exact-path matches). `includes`/`excludes` are pre-parsed segment lists.
pub(crate) fn discover<N: WalkNode>(
    root: &N,
    includes: &[Vec<String>],
    excludes: &[Vec<String>],
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for inc in includes {
        let segs = collapse_stars(inc);
        walk(root, &[], &segs, &mut out)?;
    }

    out.retain(|p| {
        let seg: Vec<&str> = p.split('.').collect();
        !excludes
            .iter()
            .any(|ex| ex.len() == seg.len() && ex.iter().zip(&seg).all(|(a, b)| a == b))
    });
    out.sort();
    out.dedup();

    Ok(out)
}

/// Discover from raw wildcard strings: parse each into include/exclude segment
/// lists, then run [`discover`].
pub(crate) fn discover_patterns<N: WalkNode>(root: &N, wildcards: &[String]) -> Result<Vec<String>> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for w in wildcards {
        let (exclude, segs) = parse_pattern(w);
        if exclude {
            excludes.push(segs)
        } else {
            includes.push(segs)
        }
    }

    discover(root, &includes, &excludes)
}

/// Split `includes` into disjoint sub-patterns for memory-bounded, parallel
/// discovery: descend each pattern's literal prefix, then expand its **first**
/// wildcard into one concrete shard per matched child — mirroring [`walk`]'s
/// `*`/`#`/opaque/recover-one-level branch logic so the union of `discover` over
/// the shards equals `discover` over the original pattern. Forcing past the first
/// wildcard is left to each shard's worker, so a `system`-level wildcard yields
/// one shard per system, each sized to a single eval-worker's RAM budget. A
/// wildcard-free pattern yields itself unchanged.
pub(crate) fn plan_shards<N: WalkNode>(root: &N, includes: &[Vec<String>]) -> Result<Vec<Vec<String>>> {
    let mut shards = Vec::new();
    for inc in includes {
        let segs = collapse_stars(inc);
        plan_one(root, &[], &segs, &mut shards)?;
    }

    Ok(shards)
}

/// One include's split. Recurses through literal segments; at the first wildcard
/// emits a shard per matched child instead of recursing into it.
fn plan_one<N: WalkNode>(
    node: &N,
    path: &[String],
    segs: &[String],
    out: &mut Vec<Vec<String>>,
) -> Result<()> {
    match segs.split_first() {
        None => out.push(path.to_vec()),
        Some((seg, rest)) if seg == "*" => {
            for name in node.child_names()? {
                let Some(child) = node.child(&name)? else {
                    continue;
                };
                let mut p = path.to_vec();
                p.push(name);

                if rest.is_empty() {
                    if child.is_derivation()? {
                        out.push(p);
                    } else if child.is_opaque()? {
                        continue;
                    } else {
                        p.push("#".to_string());
                        out.push(p);
                    }
                } else if child.is_opaque()? {
                    continue;
                } else {
                    p.extend_from_slice(rest);
                    out.push(p);
                }
            }
        }
        Some((seg, rest)) if seg == "#" => {
            for name in node.child_names()? {
                let Some(child) = node.child(&name)? else {
                    continue;
                };
                let mut p = path.to_vec();
                p.push(name);

                if rest.is_empty() {
                    if child.is_derivation()? {
                        out.push(p);
                    }
                } else {
                    p.extend_from_slice(rest);
                    out.push(p);
                }
            }
        }
        Some((seg, rest)) => {
            if let Some(child) = node.child(seg)? {
                let mut p = path.to_vec();
                p.push(seg.clone());
                plan_one(&child, &p, rest, out)?;
            }
        }
    }

    Ok(())
}

/// Render shard segments back to a pattern string for the wire: `*`/`#` stay
/// bare; a literal segment with a `.` or `"` is double-quoted so [`parse_pattern`]
/// rebuilds the same segments.
pub(crate) fn segments_to_pattern(segs: &[String]) -> String {
    segs.iter()
        .map(|s| {
            if s == "*" || s == "#" || !(s.contains('.') || s.contains('"')) {
                s.clone()
            } else {
                format!("\"{s}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct StubNode {
        derivation: bool,
        opaque: bool,
        children: BTreeMap<String, StubNode>,
    }

    impl StubNode {
        fn drv() -> Self {
            StubNode {
                derivation: true,
                opaque: false,
                children: BTreeMap::new(),
            }
        }

        fn set(children: Vec<(&str, StubNode)>) -> Self {
            StubNode {
                derivation: false,
                opaque: false,
                children: children.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            }
        }

        fn opaque(children: Vec<(&str, StubNode)>) -> Self {
            StubNode {
                opaque: true,
                ..StubNode::set(children)
            }
        }
    }

    impl WalkNode for &StubNode {
        fn child_names(&self) -> Result<Vec<String>> {
            Ok(self.children.keys().cloned().collect())
        }

        fn child(&self, name: &str) -> Result<Option<Self>> {
            Ok(self.children.get(name))
        }

        fn is_derivation(&self) -> Result<bool> {
            Ok(self.derivation)
        }

        fn is_opaque(&self) -> Result<bool> {
            Ok(self.opaque)
        }
    }

    fn tree() -> StubNode {
        StubNode::set(vec![
            (
                "packages",
                StubNode::set(vec![
                    (
                        "x86_64-linux",
                        StubNode::set(vec![
                            ("hello", StubNode::drv()),
                            ("cowsay", StubNode::drv()),
                            ("nested", StubNode::set(vec![("inner", StubNode::drv())])),
                        ]),
                    ),
                    (
                        "aarch64-linux",
                        StubNode::set(vec![("hello", StubNode::drv())]),
                    ),
                ]),
            ),
            (
                "checks",
                StubNode::set(vec![(
                    "x86_64-linux",
                    StubNode::set(vec![("test", StubNode::drv())]),
                )]),
            ),
        ])
    }

    fn segs(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_pattern_exclude() {
        assert_eq!(
            parse_pattern("!packages.x86_64-linux.broken"),
            (true, segs(&["packages", "x86_64-linux", "broken"]))
        );
    }

    #[test]
    fn parse_pattern_include_wildcard() {
        assert_eq!(parse_pattern("packages.*"), (false, segs(&["packages", "*"])));
    }

    #[test]
    fn parse_pattern_quoted_segment() {
        assert_eq!(
            parse_pattern(r#"packages.x86_64-linux."python3.12".*"#),
            (false, segs(&["packages", "x86_64-linux", "python3.12", "*"]))
        );
        assert_eq!(parse_pattern(r#"!a."b.c""#), (true, segs(&["a", "b.c"])));
    }

    #[test]
    fn collapse_consecutive_stars() {
        assert_eq!(
            collapse_stars(&segs(&["packages", "*", "*"])),
            segs(&["packages", "*"])
        );
    }

    #[test]
    fn discover_double_star_recovers_one_level() {
        let root = tree();
        let got = discover(&&root, &[segs(&["packages", "*", "*"])], &[]).unwrap();
        assert_eq!(
            got,
            vec![
                "packages.aarch64-linux.hello",
                "packages.x86_64-linux.cowsay",
                "packages.x86_64-linux.hello",
            ]
        );
    }

    #[test]
    fn discover_hash_non_recursive() {
        let root = tree();
        let got = discover(&&root, &[segs(&["packages", "x86_64-linux", "#"])], &[]).unwrap();
        assert_eq!(
            got,
            vec!["packages.x86_64-linux.cowsay", "packages.x86_64-linux.hello"]
        );
    }

    #[test]
    fn discover_literal() {
        let root = tree();
        let got = discover(&&root, &[segs(&["packages", "x86_64-linux", "hello"])], &[]).unwrap();
        assert_eq!(got, vec!["packages.x86_64-linux.hello"]);
    }

    #[test]
    fn discover_with_exclude() {
        let root = tree();
        let got = discover(
            &&root,
            &[segs(&["packages", "*"])],
            &[segs(&["packages", "aarch64-linux", "hello"])],
        )
        .unwrap();
        assert_eq!(
            got,
            vec!["packages.x86_64-linux.cowsay", "packages.x86_64-linux.hello"]
        );
    }

    #[test]
    fn discover_checks_star() {
        let root = tree();
        let got = discover(&&root, &[segs(&["checks", "*"])], &[]).unwrap();
        assert_eq!(got, vec!["checks.x86_64-linux.test"]);
    }

    #[test]
    fn discover_hash_non_last_recurses() {
        let root = StubNode::set(vec![(
            "top",
            StubNode::set(vec![
                ("x", StubNode::set(vec![("leaf", StubNode::drv())])),
                ("y", StubNode::set(vec![("leaf", StubNode::drv())])),
            ]),
        )]);
        let got = discover(&&root, &[segs(&["top", "#", "leaf"])], &[]).unwrap();
        assert_eq!(got, vec!["top.x.leaf", "top.y.leaf"]);
    }

    #[test]
    fn discover_hash_terminal_non_recursive() {
        let root = StubNode::set(vec![(
            "top",
            StubNode::set(vec![
                ("a", StubNode::drv()),
                ("nested", StubNode::set(vec![("inner", StubNode::drv())])),
            ]),
        )]);
        let got = discover(&&root, &[segs(&["top", "#"])], &[]).unwrap();
        assert_eq!(got, vec!["top.a"]);
    }

    #[test]
    fn discover_star_non_last_stops_at_opaque() {
        // `*` iterates an opaque child (sysA) and a normal child (sysB); the
        // opaque one must not be descended, so only sysB's leaf is emitted.
        let root = StubNode::set(vec![(
            "packages",
            StubNode::set(vec![
                ("sysA", StubNode::opaque(vec![("hello", StubNode::drv())])),
                ("sysB", StubNode::set(vec![("hello", StubNode::drv())])),
            ]),
        )]);
        let got = discover(&&root, &[segs(&["packages", "*", "hello"])], &[]).unwrap();
        assert_eq!(got, vec!["packages.sysB.hello"]);
    }

    #[test]
    fn discover_trailing_star_stops_at_opaque() {
        let root = StubNode::set(vec![(
            "top",
            StubNode::set(vec![
                ("realset", StubNode::set(vec![("a", StubNode::drv())])),
                ("optset", StubNode::opaque(vec![("b", StubNode::drv())])),
            ]),
        )]);
        let got = discover(&&root, &[segs(&["top", "*", "*"])], &[]).unwrap();
        assert_eq!(got, vec!["top.realset.a"]);
    }

    #[test]
    fn discover_trailing_star_emits_derivation_child() {
        let root = StubNode::set(vec![("top", StubNode::set(vec![("d", StubNode::drv())]))]);
        let got = discover(&&root, &[segs(&["top", "*"])], &[]).unwrap();
        assert_eq!(got, vec!["top.d"]);
    }

    // ── plan_shards ──────────────────────────────────────────────────────────

    fn shards(root: &StubNode, pattern: &[&str]) -> Vec<Vec<String>> {
        plan_shards(&root, &[segs(pattern)]).unwrap()
    }

    /// The contract that justifies the whole split: discovering each shard and
    /// unioning must equal discovering the original pattern in one pass.
    fn assert_split_equivalent(root: &StubNode, pattern: &[&str]) {
        let original = discover(&root, &[segs(pattern)], &[]).unwrap();

        let mut union = Vec::new();
        for shard in plan_shards(&root, &[segs(pattern)]).unwrap() {
            union.extend(discover(&root, &[shard], &[]).unwrap());
        }
        union.sort();
        union.dedup();

        assert_eq!(union, original, "split of {pattern:?} must match one-pass discover");
    }

    #[test]
    fn plan_trailing_star_splits_per_child_with_recover_shard() {
        let root = tree();
        assert_eq!(shards(&root, &["packages", "*", "*"]), vec![
            segs(&["packages", "aarch64-linux", "#"]),
            segs(&["packages", "x86_64-linux", "#"]),
        ]);
        assert_split_equivalent(&root, &["packages", "*", "*"]);
    }

    #[test]
    fn plan_non_trailing_wildcard_keeps_residual() {
        let root = tree();
        assert_eq!(shards(&root, &["packages", "*", "hello"]), vec![
            segs(&["packages", "aarch64-linux", "hello"]),
            segs(&["packages", "x86_64-linux", "hello"]),
        ]);
        assert_split_equivalent(&root, &["packages", "*", "hello"]);
    }

    #[test]
    fn plan_hash_terminal_splits_only_derivation_children() {
        let root = tree();
        assert_eq!(shards(&root, &["packages", "x86_64-linux", "#"]), vec![
            segs(&["packages", "x86_64-linux", "cowsay"]),
            segs(&["packages", "x86_64-linux", "hello"]),
        ]);
        assert_split_equivalent(&root, &["packages", "x86_64-linux", "#"]);
    }

    #[test]
    fn plan_literal_pattern_passes_through() {
        let root = tree();
        assert_eq!(shards(&root, &["packages", "x86_64-linux", "hello"]), vec![segs(
            &["packages", "x86_64-linux", "hello"]
        )]);
    }

    #[test]
    fn plan_missing_prefix_yields_no_shards() {
        let root = tree();
        assert!(shards(&root, &["nope", "*"]).is_empty());
    }

    #[test]
    fn plan_skips_opaque_under_wildcard() {
        let root = StubNode::set(vec![(
            "packages",
            StubNode::set(vec![
                ("sysA", StubNode::opaque(vec![("hello", StubNode::drv())])),
                ("sysB", StubNode::set(vec![("hello", StubNode::drv())])),
            ]),
        )]);
        assert_eq!(shards(&root, &["packages", "*", "hello"]), vec![segs(&[
            "packages", "sysB", "hello"
        ])]);
        assert_split_equivalent(&root, &["packages", "*", "hello"]);
        assert_split_equivalent(&root, &["packages", "*", "*"]);
    }

    #[test]
    fn plan_top_level_wildcard_shards_by_category() {
        let root = tree();
        assert_split_equivalent(&root, &["*", "*", "*"]);
    }

    #[test]
    fn plan_multiple_includes_concatenate() {
        let root = tree();
        let got = plan_shards(&&root, &[segs(&["packages", "*", "*"]), segs(&["checks", "*", "*"])])
            .unwrap();
        assert_eq!(got, vec![
            segs(&["packages", "aarch64-linux", "#"]),
            segs(&["packages", "x86_64-linux", "#"]),
            segs(&["checks", "x86_64-linux", "#"]),
        ]);
    }

    #[test]
    fn segments_to_pattern_quotes_dotted_segments_only() {
        assert_eq!(segments_to_pattern(&segs(&["packages", "x86_64-linux", "#"])), "packages.x86_64-linux.#");
        assert_eq!(segments_to_pattern(&segs(&["packages", "*"])), "packages.*");
        let dotted = segs(&["packages", "x86_64-linux", "python3.12"]);
        let rendered = segments_to_pattern(&dotted);
        assert_eq!(rendered, r#"packages.x86_64-linux."python3.12""#);
        assert_eq!(parse_pattern(&rendered).1, dotted);
    }
}
