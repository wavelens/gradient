/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;

pub trait WalkNode: Sized {
    fn child_names(&self) -> Result<Vec<String>>;
    fn child(&self, name: &str) -> Result<Option<Self>>;
    fn is_derivation(&self) -> Result<bool>;
    /// `*` traversal must not descend into an opaque typed attrset like a NixOS option.
    fn is_opaque(&self) -> Result<bool>;
}

pub fn collapse_stars(segs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in segs {
        if s == "*" && out.last().map(|p| p == "*").unwrap_or(false) {
            continue;
        }

        out.push(s.clone());
    }

    out
}

pub fn parse_pattern(pat: &str) -> (bool, Vec<String>) {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    pub segments: Vec<String>,
    pub only: Option<Vec<String>>,
}

/// Discovery and shard planning are sharing one [`traverse`] through this sink.
/// The split-then-union invariant is holding by construction instead of by test.
enum Sink<'a> {
    Derivations {
        out: &'a mut Vec<String>,
        deferred: Option<&'a mut Vec<Shard>>,
    },
    Shards(&'a mut Vec<Shard>),
}

impl Sink<'_> {
    fn emit_leaf(&mut self, path: Vec<String>) {
        match self {
            Sink::Derivations { out, .. } => out.push(path.join(".")),
            Sink::Shards(out) => out.push(Shard {
                segments: path,
                only: None,
            }),
        }
    }

    fn restrict_trailing(&mut self, path: &[String], wildcard: &str, names: Vec<String>) -> bool {
        let Sink::Shards(out) = self else {
            return false;
        };

        if !names.is_empty() {
            let mut segments = path.to_vec();
            segments.push(wildcard.to_owned());
            out.push(Shard {
                segments,
                only: Some(names),
            });
        }

        true
    }

    fn defer_children(&mut self, path: &[String], names: &[String]) -> bool {
        let Sink::Derivations {
            deferred: Some(deferred),
            ..
        } = self
        else {
            return false;
        };

        if !names.is_empty() {
            let mut segments = path.to_vec();
            segments.push("#".to_owned());
            deferred.push(Shard {
                segments,
                only: Some(names.to_vec()),
            });
        }

        true
    }
}

fn wildcard_children<N: WalkNode>(
    node: &N,
    path: &[String],
    only: Option<&[String]>,
    diags: &mut Vec<String>,
) -> Vec<String> {
    let names = tolerate(node.child_names(), path, diags);
    match only {
        Some(only) => names.into_iter().filter(|n| only.contains(n)).collect(),
        None => names,
    }
}

fn tolerate<T: Default>(res: Result<T>, path: &[String], diags: &mut Vec<String>) -> T {
    match res {
        Ok(v) => v,
        Err(e) => {
            diags.push(format!("failed to evaluate '{}': {:#}", path.join("."), e));
            T::default()
        }
    }
}

fn traverse<N: WalkNode>(
    node: &N,
    path: &[String],
    segs: &[String],
    only: Option<&[String]>,
    sink: &mut Sink<'_>,
    diags: &mut Vec<String>,
) {
    match segs.split_first() {
        None => match sink {
            Sink::Derivations { out, .. } => {
                if tolerate(node.is_derivation(), path, diags) {
                    out.push(path.join("."));
                }
            }
            Sink::Shards(_) => sink.emit_leaf(path.to_vec()),
        },
        Some((seg, rest)) if seg == "*" => {
            let names = wildcard_children(node, path, only, diags);
            if rest.is_empty() && sink.restrict_trailing(path, seg, names.clone()) {
                return;
            }

            for name in names {
                let mut p = path.to_vec();
                p.push(name.clone());
                let Some(child) = tolerate(node.child(&name), &p, diags) else {
                    continue;
                };

                if rest.is_empty() {
                    if tolerate(child.is_derivation(), &p, diags) {
                        sink.emit_leaf(p);
                    } else if tolerate(child.is_opaque(), &p, diags) {
                        continue;
                    } else {
                        let subs = tolerate(child.child_names(), &p, diags);
                        if sink.defer_children(&p, &subs) {
                            continue;
                        }

                        for sub in subs {
                            let mut q = p.clone();
                            q.push(sub.clone());
                            let Some(gc) = tolerate(child.child(&sub), &q, diags) else {
                                continue;
                            };
                            if tolerate(gc.is_derivation(), &q, diags) {
                                sink.emit_leaf(q);
                            }
                        }
                    }
                } else if tolerate(child.is_opaque(), &p, diags) {
                    continue;
                } else {
                    descend(&child, p, rest, sink, diags);
                }
            }
        }
        Some((seg, rest)) if seg == "#" => {
            let names = wildcard_children(node, path, only, diags);
            if rest.is_empty() && sink.restrict_trailing(path, seg, names.clone()) {
                return;
            }

            for name in names {
                let mut p = path.to_vec();
                p.push(name.clone());
                let Some(child) = tolerate(node.child(&name), &p, diags) else {
                    continue;
                };

                if rest.is_empty() {
                    if tolerate(child.is_derivation(), &p, diags) {
                        sink.emit_leaf(p);
                    }
                } else {
                    descend(&child, p, rest, sink, diags);
                }
            }
        }
        Some((seg, rest)) => {
            let mut p = path.to_vec();
            p.push(seg.clone());
            if let Some(child) = tolerate(node.child(seg), &p, diags) {
                traverse(&child, &p, rest, only, sink, diags);
            }
        }
    }
}

fn descend<N: WalkNode>(
    child: &N,
    mut path: Vec<String>,
    rest: &[String],
    sink: &mut Sink<'_>,
    diags: &mut Vec<String>,
) {
    match sink {
        Sink::Shards(out) => {
            path.extend_from_slice(rest);
            out.push(Shard {
                segments: path,
                only: None,
            });
        }
        Sink::Derivations { .. } => traverse(child, &path, rest, None, sink, diags),
    }
}

pub fn discover<N: WalkNode>(
    root: &N,
    includes: &[Vec<String>],
    excludes: &[Vec<String>],
) -> (Vec<String>, Vec<String>) {
    discover_within(root, includes, excludes, None)
}

pub fn discover_within<N: WalkNode>(
    root: &N,
    includes: &[Vec<String>],
    excludes: &[Vec<String>],
    only: Option<&[String]>,
) -> (Vec<String>, Vec<String>) {
    collect_derivations(root, includes, excludes, only, None)
}

pub fn discover_split<N: WalkNode>(
    root: &N,
    includes: &[Vec<String>],
    excludes: &[Vec<String>],
    only: Option<&[String]>,
) -> (Vec<String>, Vec<Shard>, Vec<String>) {
    let mut deferred = Vec::new();
    let (out, diags) = collect_derivations(root, includes, excludes, only, Some(&mut deferred));

    (out, deferred, diags)
}

fn collect_derivations<N: WalkNode>(
    root: &N,
    includes: &[Vec<String>],
    excludes: &[Vec<String>],
    only: Option<&[String]>,
    mut deferred: Option<&mut Vec<Shard>>,
) -> (Vec<String>, Vec<String>) {
    let mut out = Vec::new();
    let mut diags = Vec::new();
    for inc in includes {
        let segs = collapse_stars(inc);
        traverse(
            root,
            &[],
            &segs,
            only,
            &mut Sink::Derivations {
                out: &mut out,
                deferred: deferred.as_deref_mut(),
            },
            &mut diags,
        );
    }

    out.retain(|p| {
        let seg: Vec<&str> = p.split('.').collect();
        !excludes
            .iter()
            .any(|ex| ex.len() == seg.len() && ex.iter().zip(&seg).all(|(a, b)| a == b))
    });
    out.sort();
    out.dedup();
    diags.sort();
    diags.dedup();

    (out, diags)
}

pub fn parse_patterns(wildcards: &[String]) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
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

    (includes, excludes)
}

pub fn plan_shards<N: WalkNode>(root: &N, includes: &[Vec<String>]) -> (Vec<Shard>, Vec<String>) {
    let mut shards = Vec::new();
    let mut diags = Vec::new();
    for inc in includes {
        let segs = collapse_stars(inc);
        traverse(
            root,
            &[],
            &segs,
            None,
            &mut Sink::Shards(&mut shards),
            &mut diags,
        );
    }
    diags.sort();
    diags.dedup();

    (shards, diags)
}

pub fn segments_to_pattern(segs: &[String]) -> String {
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
        throws: bool,
        children: BTreeMap<String, StubNode>,
    }

    impl StubNode {
        fn drv() -> Self {
            StubNode {
                derivation: true,
                opaque: false,
                throws: false,
                children: BTreeMap::new(),
            }
        }

        fn set(children: Vec<(&str, StubNode)>) -> Self {
            StubNode {
                derivation: false,
                opaque: false,
                throws: false,
                children: children
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            }
        }

        fn opaque(children: Vec<(&str, StubNode)>) -> Self {
            StubNode {
                opaque: true,
                ..StubNode::set(children)
            }
        }

        fn throwing() -> Self {
            StubNode {
                derivation: false,
                opaque: false,
                throws: true,
                children: BTreeMap::new(),
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
            if self.throws {
                anyhow::bail!("boom");
            }
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
        assert_eq!(
            parse_pattern("packages.*"),
            (false, segs(&["packages", "*"]))
        );
    }

    #[test]
    fn parse_pattern_quoted_segment() {
        assert_eq!(
            parse_pattern(r#"packages.x86_64-linux."python3.12".*"#),
            (
                false,
                segs(&["packages", "x86_64-linux", "python3.12", "*"])
            )
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
        let got = discover(&&root, &[segs(&["packages", "*", "*"])], &[]).0;
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
        let got = discover(&&root, &[segs(&["packages", "x86_64-linux", "#"])], &[]).0;
        assert_eq!(
            got,
            vec![
                "packages.x86_64-linux.cowsay",
                "packages.x86_64-linux.hello"
            ]
        );
    }

    #[test]
    fn discover_literal() {
        let root = tree();
        let got = discover(&&root, &[segs(&["packages", "x86_64-linux", "hello"])], &[]).0;
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
        .0;
        assert_eq!(
            got,
            vec![
                "packages.x86_64-linux.cowsay",
                "packages.x86_64-linux.hello"
            ]
        );
    }

    #[test]
    fn discover_checks_star() {
        let root = tree();
        let got = discover(&&root, &[segs(&["checks", "*"])], &[]).0;
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
        let got = discover(&&root, &[segs(&["top", "#", "leaf"])], &[]).0;
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
        let got = discover(&&root, &[segs(&["top", "#"])], &[]).0;
        assert_eq!(got, vec!["top.a"]);
    }

    #[test]
    fn discover_star_non_last_stops_at_opaque() {
        let root = StubNode::set(vec![(
            "packages",
            StubNode::set(vec![
                ("sysA", StubNode::opaque(vec![("hello", StubNode::drv())])),
                ("sysB", StubNode::set(vec![("hello", StubNode::drv())])),
            ]),
        )]);
        let got = discover(&&root, &[segs(&["packages", "*", "hello"])], &[]).0;
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
        let got = discover(&&root, &[segs(&["top", "*", "*"])], &[]).0;
        assert_eq!(got, vec!["top.realset.a"]);
    }

    #[test]
    fn discover_trailing_star_emits_derivation_child() {
        let root = StubNode::set(vec![("top", StubNode::set(vec![("d", StubNode::drv())]))]);
        let got = discover(&&root, &[segs(&["top", "*"])], &[]).0;
        assert_eq!(got, vec!["top.d"]);
    }

    #[test]
    fn traverse_records_thrown_attr_and_continues() {
        let root = StubNode::set(vec![("ok", StubNode::drv()), ("bad", StubNode::throwing())]);
        let (got, errors) = discover(&&root, &[segs(&["*"])], &[]);
        assert_eq!(got, vec!["ok"], "sibling still discovered");
        assert_eq!(errors.len(), 1, "one dedup'd diagnostic: {errors:?}");
        assert!(
            errors[0].contains("bad") && errors[0].contains("boom"),
            "diagnostic names the attr and the nix error: {errors:?}"
        );
    }

    #[test]
    fn traverse_records_full_dotted_path() {
        let root = StubNode::set(vec![(
            "packages",
            StubNode::set(vec![(
                "x86_64-linux",
                StubNode::set(vec![
                    ("hello", StubNode::drv()),
                    ("broken", StubNode::throwing()),
                ]),
            )]),
        )]);
        let (got, errors) = discover(&&root, &[segs(&["packages", "x86_64-linux", "*"])], &[]);
        assert_eq!(got, vec!["packages.x86_64-linux.hello"]);
        assert!(
            errors
                .iter()
                .any(|e| e.contains("packages.x86_64-linux.broken")),
            "path is the full dotted attr path: {errors:?}"
        );
    }

    fn shards(root: &StubNode, pattern: &[&str]) -> Vec<Shard> {
        plan_shards(&root, &[segs(pattern)]).0
    }

    fn residual(parts: &[&str]) -> Shard {
        Shard {
            segments: segs(parts),
            only: None,
        }
    }

    fn restricted(parts: &[&str], names: &[&str]) -> Shard {
        Shard {
            segments: segs(parts),
            only: Some(segs(names)),
        }
    }

    fn assert_split_equivalent(root: &StubNode, pattern: &[&str]) {
        let original = discover(&root, &[segs(pattern)], &[]).0;

        let mut union = Vec::new();
        let mut queue = plan_shards(&root, &[segs(pattern)]).0;
        while let Some(shard) = queue.pop() {
            let names = shard.only.clone().unwrap_or_default();
            let batches: Vec<Option<&[String]>> = match shard.only {
                Some(_) => names.iter().map(std::slice::from_ref).map(Some).collect(),
                None => vec![None],
            };
            for only in batches {
                let (attrs, deferred, _) =
                    discover_split(&root, std::slice::from_ref(&shard.segments), &[], only);
                union.extend(attrs);
                queue.extend(deferred);
            }
        }
        union.sort();
        union.dedup();

        assert_eq!(
            union, original,
            "split of {pattern:?} must match one-pass discover"
        );
    }

    #[test]
    fn plan_trailing_star_restricts_the_pattern_to_each_child_name() {
        let root = tree();
        assert_eq!(
            shards(&root, &["packages", "*", "*"]),
            vec![restricted(
                &["packages", "*"],
                &["aarch64-linux", "x86_64-linux"]
            )]
        );
    }

    #[test]
    fn plan_trailing_wildcard_forces_no_child() {
        let root = StubNode::set(vec![(
            "hydraJobs",
            StubNode::set(vec![
                ("a", StubNode::throwing()),
                ("b", StubNode::throwing()),
            ]),
        )]);
        for wildcard in ["*", "#"] {
            let (got, errors) = plan_shards(&&root, &[segs(&["hydraJobs", wildcard])]);
            assert_eq!(got, vec![restricted(&["hydraJobs", wildcard], &["a", "b"])]);
            assert!(errors.is_empty(), "no child was forced: {errors:?}");
        }
    }

    #[test]
    fn restricted_shards_match_one_pass_discovery() {
        let root = StubNode::set(vec![(
            "jobs",
            StubNode::set(vec![
                ("drv", StubNode::drv()),
                ("set", StubNode::set(vec![("inner", StubNode::drv())])),
                ("opt", StubNode::opaque(vec![("hidden", StubNode::drv())])),
                ("bad", StubNode::throwing()),
            ]),
        )]);
        assert_split_equivalent(&root, &["jobs", "*"]);
        assert_split_equivalent(&root, &["jobs", "#"]);
        assert_split_equivalent(&root, &["*", "*"]);
    }

    #[test]
    fn split_discovery_defers_each_nested_set_under_a_trailing_star() {
        let root = tree();
        let (attrs, deferred, _) = discover_split(
            &&root,
            &[segs(&["packages", "*"])],
            &[],
            Some(&segs(&["x86_64-linux"])),
        );
        assert!(
            attrs.is_empty(),
            "no grandchild is forced inline: {attrs:?}"
        );
        assert_eq!(
            deferred,
            vec![restricted(
                &["packages", "x86_64-linux", "#"],
                &["cowsay", "hello", "nested"]
            )]
        );

        let (attrs, deferred, _) = discover_split(
            &&root,
            &[segs(&["packages", "x86_64-linux", "*"])],
            &[],
            None,
        );
        assert_eq!(
            attrs,
            vec![
                "packages.x86_64-linux.cowsay",
                "packages.x86_64-linux.hello"
            ]
        );
        assert_eq!(
            deferred,
            vec![restricted(
                &["packages", "x86_64-linux", "nested", "#"],
                &["inner"]
            )]
        );
    }

    #[test]
    fn plan_non_trailing_wildcard_keeps_residual() {
        let root = tree();
        assert_eq!(
            shards(&root, &["packages", "*", "hello"]),
            vec![
                residual(&["packages", "aarch64-linux", "hello"]),
                residual(&["packages", "x86_64-linux", "hello"]),
            ]
        );
    }

    #[test]
    fn plan_hash_terminal_restricts_the_pattern_to_each_child_name() {
        let root = tree();
        assert_eq!(
            shards(&root, &["packages", "x86_64-linux", "#"]),
            vec![restricted(
                &["packages", "x86_64-linux", "#"],
                &["cowsay", "hello", "nested"]
            )]
        );
        assert_split_equivalent(&root, &["packages", "x86_64-linux", "#"]);
    }

    #[test]
    fn plan_literal_pattern_passes_through() {
        let root = tree();
        assert_eq!(
            shards(&root, &["packages", "x86_64-linux", "hello"]),
            vec![residual(&["packages", "x86_64-linux", "hello"])]
        );
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
        assert_eq!(
            shards(&root, &["packages", "*", "hello"]),
            vec![residual(&["packages", "sysB", "hello"])]
        );
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
        let got = plan_shards(
            &&root,
            &[segs(&["packages", "*", "*"]), segs(&["checks", "*", "*"])],
        )
        .0;
        assert_eq!(
            got,
            vec![
                restricted(&["packages", "*"], &["aarch64-linux", "x86_64-linux"]),
                restricted(&["checks", "*"], &["x86_64-linux"]),
            ]
        );
    }

    #[test]
    fn segments_to_pattern_quotes_dotted_segments_only() {
        assert_eq!(
            segments_to_pattern(&segs(&["packages", "x86_64-linux", "#"])),
            "packages.x86_64-linux.#"
        );
        assert_eq!(segments_to_pattern(&segs(&["packages", "*"])), "packages.*");
        let dotted = segs(&["packages", "x86_64-linux", "python3.12"]);
        let rendered = segments_to_pattern(&dotted);
        assert_eq!(rendered, r#"packages.x86_64-linux."python3.12""#);
        assert_eq!(parse_pattern(&rendered).1, dotted);
    }
}
