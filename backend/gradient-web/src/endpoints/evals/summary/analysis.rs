/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::ids::DerivationId;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

pub(super) struct Package {
    pub attr: String,
    pub derivation: DerivationId,
}

pub(super) struct EvaluationGraph {
    pub statuses: HashMap<DerivationId, BuildStatus>,
    pub edges: Vec<(DerivationId, DerivationId)>,
    pub packages: Vec<Package>,
}

pub(super) struct Baseline {
    pub broken: HashSet<String>,
    pub failed_derivations: HashSet<DerivationId>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Failure {
    pub derivation: DerivationId,
    pub status: BuildStatus,
    pub attributes: Vec<String>,
    pub blocked: Vec<String>,
    pub newly_failing: bool,
}

#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub struct PackageCounts {
    pub total: usize,
    pub built: usize,
    pub failed: usize,
    pub blocked: usize,
    pub unfinished: usize,
}

const fn failed_itself(status: BuildStatus) -> bool {
    matches!(
        status,
        BuildStatus::FailedPermanent | BuildStatus::FailedTransient | BuildStatus::FailedTimeout
    )
}

impl Baseline {
    pub(super) fn of(graph: &EvaluationGraph, failed_attributes: Vec<String>) -> Self {
        let broken_packages = graph
            .packages
            .iter()
            .filter(|p| {
                graph
                    .statuses
                    .get(&p.derivation)
                    .is_some_and(|s| s.is_failure())
            })
            .map(|p| p.attr.clone());

        Self {
            broken: broken_packages.chain(failed_attributes).collect(),
            failed_derivations: graph
                .statuses
                .iter()
                .filter(|(_, status)| failed_itself(**status))
                .map(|(derivation, _)| *derivation)
                .collect(),
        }
    }
}

pub(super) fn package_counts(graph: &EvaluationGraph) -> PackageCounts {
    let mut counts = PackageCounts {
        total: graph.packages.len(),
        ..PackageCounts::default()
    };
    for package in &graph.packages {
        match graph.statuses.get(&package.derivation) {
            Some(status) if status.is_terminal_success() => counts.built += 1,
            Some(status) if failed_itself(*status) => counts.failed += 1,
            Some(BuildStatus::DependencyFailed) => counts.blocked += 1,
            _ => counts.unfinished += 1,
        }
    }

    counts
}

/// Builds that failed themselves, the newly failing and the most blocking first.
pub(super) fn failures(graph: &EvaluationGraph, baseline: Option<&Baseline>) -> Vec<Failure> {
    let dependents = dependents_of(&graph.edges);
    let attributes = attributes_by_derivation(&graph.packages);
    let mut failures: Vec<Failure> = graph
        .statuses
        .iter()
        .filter(|(_, status)| failed_itself(**status))
        .map(|(derivation, status)| {
            let own = attributes.get(derivation).cloned().unwrap_or_default();
            let blocked = blocked_attributes(*derivation, graph, &dependents, &attributes);
            let newly_failing =
                baseline.is_some_and(|before| !failed_before(before, *derivation, &own, &blocked));

            Failure {
                derivation: *derivation,
                status: *status,
                attributes: own,
                blocked,
                newly_failing,
            }
        })
        .collect();
    failures.sort_by(|a, b| {
        b.newly_failing
            .cmp(&a.newly_failing)
            .then_with(|| b.blocked.len().cmp(&a.blocked.len()))
            .then_with(|| a.attributes.cmp(&b.attributes))
            .then_with(|| a.derivation.cmp(&b.derivation))
    });

    failures
}

pub(super) fn fixed(graph: &EvaluationGraph, baseline: &Baseline) -> Vec<String> {
    let fixed: BTreeSet<&String> = graph
        .packages
        .iter()
        .filter(|p| baseline.broken.contains(&p.attr))
        .filter(|p| {
            graph
                .statuses
                .get(&p.derivation)
                .is_some_and(|s| s.is_terminal_success())
        })
        .map(|p| &p.attr)
        .collect();

    fixed.into_iter().cloned().collect()
}

fn failed_before(
    baseline: &Baseline,
    derivation: DerivationId,
    own: &[String],
    blocked: &[String],
) -> bool {
    let mut affected = own.iter().chain(blocked).peekable();

    baseline.failed_derivations.contains(&derivation)
        || (affected.peek().is_some() && affected.all(|attr| baseline.broken.contains(attr)))
}

fn dependents_of(
    edges: &[(DerivationId, DerivationId)],
) -> HashMap<DerivationId, Vec<DerivationId>> {
    let mut dependents: HashMap<DerivationId, Vec<DerivationId>> = HashMap::new();
    for (derivation, dependency) in edges {
        dependents.entry(*dependency).or_default().push(*derivation);
    }

    dependents
}

fn attributes_by_derivation(packages: &[Package]) -> HashMap<DerivationId, Vec<String>> {
    let mut attributes: HashMap<DerivationId, Vec<String>> = HashMap::new();
    for package in packages {
        attributes
            .entry(package.derivation)
            .or_default()
            .push(package.attr.clone());
    }
    for attrs in attributes.values_mut() {
        attrs.sort();
    }

    attributes
}

fn blocked_attributes(
    root: DerivationId,
    graph: &EvaluationGraph,
    dependents: &HashMap<DerivationId, Vec<DerivationId>>,
    attributes: &HashMap<DerivationId, Vec<String>>,
) -> Vec<String> {
    let mut seen = HashSet::from([root]);
    let mut frontier = vec![root];
    let mut blocked: BTreeSet<&String> = BTreeSet::new();
    while let Some(derivation) = frontier.pop() {
        for dependent in dependents.get(&derivation).into_iter().flatten() {
            let broken = graph
                .statuses
                .get(dependent)
                .is_some_and(|s| s.is_failure());
            if !broken || !seen.insert(*dependent) {
                continue;
            }
            blocked.extend(attributes.get(dependent).into_iter().flatten());
            frontier.push(*dependent);
        }
    }

    blocked.into_iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        glibc: DerivationId,
        curl: DerivationId,
        git: DerivationId,
        hello: DerivationId,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                glibc: DerivationId::now_v7(),
                curl: DerivationId::now_v7(),
                git: DerivationId::now_v7(),
                hello: DerivationId::now_v7(),
            }
        }

        /// `git` needs `curl`, `curl` needs `glibc`; `hello` stands alone.
        fn graph(&self, statuses: [BuildStatus; 4]) -> EvaluationGraph {
            let [glibc, curl, git, hello] = statuses;

            EvaluationGraph {
                statuses: HashMap::from([
                    (self.glibc, glibc),
                    (self.curl, curl),
                    (self.git, git),
                    (self.hello, hello),
                ]),
                edges: vec![(self.git, self.curl), (self.curl, self.glibc)],
                packages: vec![
                    Package {
                        attr: "curl".into(),
                        derivation: self.curl,
                    },
                    Package {
                        attr: "git".into(),
                        derivation: self.git,
                    },
                    Package {
                        attr: "hello".into(),
                        derivation: self.hello,
                    },
                ],
            }
        }
    }

    use BuildStatus::{Completed, DependencyFailed, FailedPermanent, Queued};

    #[test]
    fn a_failed_dependency_names_the_packages_it_blocks() {
        let fixture = Fixture::new();
        let graph = fixture.graph([
            FailedPermanent,
            DependencyFailed,
            DependencyFailed,
            Completed,
        ]);

        let failures = failures(&graph, None);

        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].derivation, fixture.glibc);
        assert!(failures[0].attributes.is_empty());
        assert_eq!(failures[0].blocked, ["curl", "git"]);
        assert!(!failures[0].newly_failing);
    }

    #[test]
    fn a_package_that_built_from_cache_is_not_blocked() {
        let fixture = Fixture::new();
        let graph = fixture.graph([FailedPermanent, DependencyFailed, Completed, Completed]);

        assert_eq!(failures(&graph, None)[0].blocked, ["curl"]);
    }

    #[test]
    fn a_failure_is_new_when_it_breaks_a_package_that_built_before() {
        let fixture = Fixture::new();
        let before = fixture.graph([Completed, FailedPermanent, DependencyFailed, Completed]);
        let now = fixture.graph([
            Completed,
            FailedPermanent,
            DependencyFailed,
            FailedPermanent,
        ]);
        let baseline = Baseline::of(&before, Vec::new());

        let failures = failures(&now, Some(&baseline));

        let by_derivation: HashMap<_, _> = failures
            .iter()
            .map(|f| (f.derivation, f.newly_failing))
            .collect();
        assert_eq!(failures[0].derivation, fixture.hello);
        assert!(by_derivation[&fixture.hello]);
        assert!(!by_derivation[&fixture.curl]);
    }

    #[test]
    fn a_changed_derivation_stays_old_while_every_package_was_broken_before() {
        let (before, now) = (Fixture::new(), Fixture::new());
        let baseline = Baseline::of(
            &before.graph([
                FailedPermanent,
                DependencyFailed,
                DependencyFailed,
                Completed,
            ]),
            Vec::new(),
        );
        let graph = now.graph([
            FailedPermanent,
            DependencyFailed,
            DependencyFailed,
            Completed,
        ]);

        assert!(!failures(&graph, Some(&baseline))[0].newly_failing);
    }

    #[test]
    fn a_package_broken_before_and_built_now_is_fixed() {
        let fixture = Fixture::new();
        let before = fixture.graph([
            FailedPermanent,
            DependencyFailed,
            DependencyFailed,
            Completed,
        ]);
        let now = fixture.graph([Completed, Completed, Queued, Completed]);
        let baseline = Baseline::of(&before, vec!["hello".into()]);

        assert_eq!(fixed(&now, &baseline), ["curl", "hello"]);
    }

    #[test]
    fn packages_count_under_the_outcome_of_the_package_build() {
        let fixture = Fixture::new();
        let graph = fixture.graph([Completed, FailedPermanent, DependencyFailed, Queued]);

        assert_eq!(
            package_counts(&graph),
            PackageCounts {
                total: 3,
                built: 0,
                failed: 1,
                blocked: 1,
                unfinished: 1
            }
        );
    }
}
