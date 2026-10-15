/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::actions::summary::escape_html;
use gradient_entity::build::BuildStatus;
use std::collections::BTreeMap;
use std::fmt::Write;

const MAX_BODY_LEN: usize = 65_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Failed,
    Built,
    Unchanged,
}

impl Outcome {
    pub(super) const fn of(status: BuildStatus, built_before_evaluation: bool) -> Option<Self> {
        if status.is_failure() {
            return Some(Self::Failed);
        }
        if !status.is_terminal_success() {
            return None;
        }
        if built_before_evaluation || matches!(status, BuildStatus::Substituted) {
            return Some(Self::Unchanged);
        }
        Some(Self::Built)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Package {
    pub attr: String,
    pub log: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct SystemPackages {
    failed: Vec<Package>,
    built: Vec<Package>,
    unchanged: Vec<Package>,
}

#[derive(Debug, Default)]
pub(super) struct Summary {
    pub task: String,
    pub commit: String,
    pub link: Option<String>,
    pub failed_to_evaluate: Vec<String>,
    pub systems: BTreeMap<String, SystemPackages>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    Every,
    FailuresOnly,
    CountsOnly,
}

impl Summary {
    pub(super) fn add(&mut self, system: &str, outcome: Outcome, package: Package) {
        let packages = self.systems.entry(system.to_owned()).or_default();
        match outcome {
            Outcome::Failed => packages.failed.push(package),
            Outcome::Built => packages.built.push(package),
            Outcome::Unchanged => packages.unchanged.push(package),
        }
    }

    pub(super) fn render(&self) -> String {
        [Detail::Every, Detail::FailuresOnly]
            .into_iter()
            .map(|detail| self.render_with(detail))
            .find(|body| body.len() <= MAX_BODY_LEN)
            .unwrap_or_else(|| self.render_with(Detail::CountsOnly))
    }

    fn render_with(&self, detail: Detail) -> String {
        let mut out = String::from("## `gradient` result\n\n");
        match &self.link {
            Some(link) => {
                let _ = writeln!(out, "Generated using [`gradient`]({link}).\n");
            }
            None => out.push_str("Generated using `gradient`.\n\n"),
        }

        let _ = writeln!(out, "Task: `{}`", self.task);
        let _ = writeln!(out, "Commit: `{}`\n", self.commit);
        out.push_str("---\n");

        let listed = detail != Detail::CountsOnly;
        let evaluation_failures: Vec<String> = self
            .failed_to_evaluate
            .iter()
            .map(|a| escape_html(a))
            .collect();
        section(
            &mut out,
            ":x:",
            "failed to evaluate",
            &evaluation_failures,
            listed,
        );

        for (system, packages) in &self.systems {
            let _ = writeln!(out, "### `{system}`");
            section(
                &mut out,
                ":x:",
                "failed to build",
                &items(&packages.failed),
                listed,
            );
            let listed = detail == Detail::Every;
            section(
                &mut out,
                ":white_check_mark:",
                "built",
                &items(&packages.built),
                listed,
            );
            section(
                &mut out,
                ":fast_forward:",
                "unchanged",
                &items(&packages.unchanged),
                listed,
            );
        }

        out
    }
}

fn items(packages: &[Package]) -> Vec<String> {
    let mut sorted = packages.to_vec();
    sorted.sort();
    sorted
        .into_iter()
        .map(|p| match p.log {
            Some(log) => format!(
                "<a href=\"{}\">{}</a>",
                escape_html(&log),
                escape_html(&p.attr)
            ),
            None => escape_html(&p.attr),
        })
        .collect()
}

fn section(out: &mut String, emoji: &str, message: &str, items: &[String], listed: bool) {
    if items.is_empty() {
        return;
    }

    let noun = if items.len() == 1 {
        "package"
    } else {
        "packages"
    };
    let _ = writeln!(out, "<details>");
    let _ = writeln!(
        out,
        "  <summary>{emoji} {} {noun} {message}:</summary>",
        items.len()
    );
    if listed {
        out.push_str("  <ul>\n");
        for item in items {
            let _ = writeln!(out, "    <li>{item}</li>");
        }
        out.push_str("  </ul>\n");
    }
    out.push_str("</details>\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(attr: &str) -> Package {
        Package {
            attr: attr.to_owned(),
            log: None,
        }
    }

    fn summary() -> Summary {
        Summary {
            task: "nixpkgs".into(),
            commit: "0123abc".into(),
            link: Some("https://ci.example/project/p/log/e".into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_shared_build_finished_before_the_evaluation_is_unchanged() {
        assert_eq!(
            Outcome::of(BuildStatus::Completed, true),
            Some(Outcome::Unchanged)
        );
        assert_eq!(
            Outcome::of(BuildStatus::Completed, false),
            Some(Outcome::Built)
        );
        assert_eq!(
            Outcome::of(BuildStatus::Substituted, false),
            Some(Outcome::Unchanged)
        );
        assert_eq!(
            Outcome::of(BuildStatus::DependencyFailed, true),
            Some(Outcome::Failed)
        );
        assert_eq!(Outcome::of(BuildStatus::Aborted, false), None);
        assert_eq!(Outcome::of(BuildStatus::Building, false), None);
    }

    #[test]
    fn the_report_follows_the_nixpkgs_review_layout() {
        let mut s = summary();
        s.failed_to_evaluate.push("broken".into());
        s.add(
            "x86_64-linux",
            Outcome::Failed,
            Package {
                attr: "foo".into(),
                log: Some("https://ci.example/log?build=1".into()),
            },
        );
        s.add("x86_64-linux", Outcome::Built, package("zed"));
        s.add("x86_64-linux", Outcome::Built, package("bar"));
        s.add("aarch64-linux", Outcome::Unchanged, package("baz"));

        assert_eq!(
            s.render(),
            "## `gradient` result

Generated using [`gradient`](https://ci.example/project/p/log/e).

Task: `nixpkgs`
Commit: `0123abc`

---
<details>
  <summary>:x: 1 package failed to evaluate:</summary>
  <ul>
    <li>broken</li>
  </ul>
</details>
### `aarch64-linux`
<details>
  <summary>:fast_forward: 1 package unchanged:</summary>
  <ul>
    <li>baz</li>
  </ul>
</details>
### `x86_64-linux`
<details>
  <summary>:x: 1 package failed to build:</summary>
  <ul>
    <li><a href=\"https://ci.example/log?build=1\">foo</a></li>
  </ul>
</details>
<details>
  <summary>:white_check_mark: 2 packages built:</summary>
  <ul>
    <li>bar</li>
    <li>zed</li>
  </ul>
</details>
"
        );
    }

    #[test]
    fn an_oversized_report_keeps_the_failures_listed() {
        let mut s = summary();
        s.add("x86_64-linux", Outcome::Failed, package("foo"));
        for i in 0..5_000 {
            s.add(
                "x86_64-linux",
                Outcome::Built,
                package(&format!("package-{i}")),
            );
        }

        let body = s.render();
        assert!(body.len() <= MAX_BODY_LEN);
        assert!(body.contains("<li>foo</li>"));
        assert!(body.contains(":white_check_mark: 5000 packages built:"));
        assert!(!body.contains("package-1"));
    }

    #[test]
    fn attribute_names_are_escaped() {
        let mut s = summary();
        s.add("x86_64-linux", Outcome::Built, package("<script>"));
        assert!(s.render().contains("<li>&lt;script&gt;</li>"));
    }
}
