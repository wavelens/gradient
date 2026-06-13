/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure wildcard-pattern traversal over a flake's output attr tree.
//!
//! Reproduces the segment semantics of the retired `eval.nix` resolver:
//! `*` (one level; trailing `*` recovers the collapsed second level), `#`
//! (non-recursive derivations at one depth), literal (exact), exclusions
//! (exact-path), and consecutive-`*` collapse. Abstracted over [`WalkNode`]
//! so it unit-tests with a stub tree.

use anyhow::Result;

/// A node in the flake-output attr tree (a real impl wraps an eval-cache AttrCursor).
pub(crate) trait WalkNode: Sized {
    /// Child attribute names (sorted).
    fn child_names(&self) -> Result<Vec<String>>;
    /// Child node by name, or `None` if absent.
    fn child(&self, name: &str) -> Result<Option<Self>>;
    /// Whether this node is a derivation.
    fn is_derivation(&self) -> Result<bool>;
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
/// (Quoted segments containing `.` are not split — keep it simple: split on `.`;
/// the server-side `Wildcard` validator already rejects malformed patterns.)
pub(crate) fn parse_pattern(pat: &str) -> (bool, Vec<String>) {
    let (exclude, body) = match pat.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, pat),
    };

    (exclude, body.split('.').map(|s| s.to_string()).collect())
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
                } else {
                    walk(&child, &p, rest, out)?;
                }
            }
        }
        Some((seg, _rest)) if seg == "#" => {
            for name in node.child_names()? {
                let Some(child) = node.child(&name)? else {
                    continue;
                };

                if child.is_derivation()? {
                    let mut p = path.to_vec();
                    p.push(name);
                    out.push(p.join("."));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct StubNode {
        derivation: bool,
        children: BTreeMap<String, StubNode>,
    }

    impl StubNode {
        fn drv() -> Self {
            StubNode {
                derivation: true,
                children: BTreeMap::new(),
            }
        }

        fn set(children: Vec<(&str, StubNode)>) -> Self {
            StubNode {
                derivation: false,
                children: children.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
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
}
