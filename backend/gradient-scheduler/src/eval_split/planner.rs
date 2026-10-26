/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeSet, HashMap, HashSet};

use gradient_db::eval_costs::TaskEvalCosts;

pub const SPLIT_MIN_GAIN: f64 = 0.9;
pub const NEAR_BEST: f64 = 1.05;
pub const SHARDS_PER_MEMBER: f64 = 4.0;
pub const MIN_SHARD_ROUND_TRIPS: f64 = 20.0;

#[derive(Debug, Clone, PartialEq)]
pub struct SplitParams {
    pub max_members: usize,
    pub subprocesses: usize,
    pub round_trip_ms: f64,
    pub companion_setup_ms: f64,
}

impl Default for SplitParams {
    fn default() -> Self {
        Self {
            max_members: 1,
            subprocesses: 1,
            round_trip_ms: 250.0,
            companion_setup_ms: 10_000.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListedSet {
    pub path: Vec<String>,
    pub children: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlannedShard {
    pub set: Vec<String>,
    pub children: Vec<String>,
    pub predicted_ms: f64,
}

pub fn choose_members(costs: &TaskEvalCosts, params: &SplitParams) -> usize {
    if costs.subtrees.is_empty() || params.max_members < 2 {
        return 1;
    }

    let listed = listed_from_history(costs);
    let wall_ms = |members: usize| {
        let shards = plan_shards(costs, &listed, members, params);
        predict_wall_ms(costs, &shards, members, params)
    };

    let alone_ms = wall_ms(1);
    let split: Vec<(usize, f64)> = (2..=params.max_members)
        .map(|members| (members, wall_ms(members)))
        .collect();
    let fastest_ms = split
        .iter()
        .map(|(_, ms)| *ms)
        .fold(f64::INFINITY, f64::min);
    let Some(&(members, ms)) = split.iter().find(|(_, ms)| *ms <= fastest_ms * NEAR_BEST) else {
        return 1;
    };

    if ms <= alone_ms * SPLIT_MIN_GAIN {
        members
    } else {
        1
    }
}

pub fn plan_shards(
    costs: &TaskEvalCosts,
    listed: &[ListedSet],
    members: usize,
    params: &SplitParams,
) -> Vec<PlannedShard> {
    let index = CostIndex::new(costs);
    let mut sets: Vec<(f64, Vec<PlannedShard>)> = listed
        .iter()
        .map(|set| {
            let children = costed_children(&index, set);
            let total_ms = children.iter().map(|(_, ms)| ms).sum();
            let target_ms = shard_target_ms(total_ms, members, params);
            (total_ms, pack_set(&set.path, children, target_ms))
        })
        .collect();
    sets.sort_by(|a, b| b.0.total_cmp(&a.0));

    sets.into_iter().flat_map(|(_, shards)| shards).collect()
}

pub fn predict_wall_ms(
    costs: &TaskEvalCosts,
    shards: &[PlannedShard],
    members: usize,
    params: &SplitParams,
) -> f64 {
    let startups: HashMap<&[String], f64> = costs
        .sets
        .iter()
        .map(|set| (set.path.as_slice(), set.startup_ms.unwrap_or(0.0)))
        .collect();

    if members <= 1 {
        one_member_wall_ms(&startups, shards, params)
    } else {
        simulate(&startups, shards, members, params)
    }
}

pub fn listed_from_history(costs: &TaskEvalCosts) -> Vec<ListedSet> {
    let paths: BTreeSet<&Vec<String>> = costs.sets.iter().map(|set| &set.path).collect();
    paths
        .iter()
        .filter(|path| !paths.iter().any(|other| is_strictly_below(path, other)))
        .map(|path| ListedSet {
            path: path.to_vec(),
            children: children_of(costs, path),
        })
        .collect()
}

fn is_strictly_below(path: &[String], ancestor: &[String]) -> bool {
    path.len() > ancestor.len() && path.starts_with(ancestor)
}

fn children_of(costs: &TaskEvalCosts, set: &[String]) -> Vec<String> {
    let subtree_paths = costs.subtrees.iter().map(|subtree| &subtree.path);
    let set_paths = costs.sets.iter().map(|other| &other.path);
    let children: BTreeSet<&String> = subtree_paths
        .chain(set_paths)
        .filter(|path| is_strictly_below(path, set))
        .map(|path| &path[set.len()])
        .collect();

    children.into_iter().cloned().collect()
}

#[derive(Debug, Clone, Copy, Default)]
struct Below {
    work_ms: f64,
    startup_ms: f64,
    subtrees: usize,
}

struct CostIndex {
    below: HashMap<Vec<String>, Below>,
    median_ms: f64,
}

impl CostIndex {
    fn new(costs: &TaskEvalCosts) -> Self {
        let mut below: HashMap<Vec<String>, Below> = HashMap::new();
        for subtree in &costs.subtrees {
            for len in 1..=subtree.path.len() {
                let entry = below.entry(subtree.path[..len].to_vec()).or_default();
                entry.work_ms += subtree.attrs * subtree.ms_per_attr;
                entry.subtrees += 1;
            }
        }

        for set in &costs.sets {
            for len in 1..=set.path.len() {
                let entry = below.entry(set.path[..len].to_vec()).or_default();
                entry.startup_ms += set.startup_ms.unwrap_or(0.0);
            }
        }

        let mut index = Self {
            below,
            median_ms: 0.0,
        };
        index.median_ms = index.median_known_child_ms(costs);
        index
    }

    fn known_ms(&self, path: &[String]) -> Option<f64> {
        self.below
            .get(path)
            .filter(|below| below.subtrees > 0)
            .map(|below| below.work_ms + below.startup_ms)
    }

    fn child_ms(&self, set: &[String], child: &str) -> f64 {
        self.known_ms(&child_path(set, child))
            .unwrap_or(self.median_ms)
    }

    fn median_known_child_ms(&self, costs: &TaskEvalCosts) -> f64 {
        let mut known: Vec<f64> = listed_from_history(costs)
            .iter()
            .flat_map(|set| {
                set.children
                    .iter()
                    .filter_map(|child| self.known_ms(&child_path(&set.path, child)))
            })
            .collect();
        known.sort_by(f64::total_cmp);

        known
            .len()
            .checked_sub(1)
            .map_or(0.0, |last| known[last / 2])
    }
}

fn child_path(set: &[String], child: &str) -> Vec<String> {
    let mut path = set.to_vec();
    path.push(child.to_owned());
    path
}

fn costed_children(index: &CostIndex, set: &ListedSet) -> Vec<(String, f64)> {
    set.children
        .iter()
        .map(|child| (child.clone(), index.child_ms(&set.path, child)))
        .collect()
}

fn shard_target_ms(total_ms: f64, members: usize, params: &SplitParams) -> f64 {
    let per_shard_ms = total_ms / (members.max(1) as f64 * SHARDS_PER_MEMBER);
    per_shard_ms.max(params.round_trip_ms * MIN_SHARD_ROUND_TRIPS)
}

fn pack_set(set: &[String], mut children: Vec<(String, f64)>, target_ms: f64) -> Vec<PlannedShard> {
    children.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut shards = Vec::new();
    let mut current = PlannedShard {
        set: set.to_vec(),
        ..Default::default()
    };
    for (child, ms) in children {
        if !current.children.is_empty() && current.predicted_ms + ms > target_ms {
            let next = PlannedShard {
                set: set.to_vec(),
                ..Default::default()
            };
            shards.push(std::mem::replace(&mut current, next));
        }

        current.children.push(child);
        current.predicted_ms += ms;
    }

    if !current.children.is_empty() {
        shards.push(current);
    }

    shards.sort_by(|a, b| b.predicted_ms.total_cmp(&a.predicted_ms));
    shards
}

fn one_member_wall_ms(
    startups: &HashMap<&[String], f64>,
    shards: &[PlannedShard],
    params: &SplitParams,
) -> f64 {
    let sets: HashSet<&[String]> = shards.iter().map(|shard| shard.set.as_slice()).collect();
    let startup_ms: f64 = sets.iter().map(|set| startup_of(startups, set)).sum();
    let work_ms: f64 = shards.iter().map(|shard| shard.predicted_ms).sum();

    startup_ms + work_ms / params.subprocesses.max(1) as f64
}

fn simulate(
    startups: &HashMap<&[String], f64>,
    shards: &[PlannedShard],
    members: usize,
    params: &SplitParams,
) -> f64 {
    let subprocesses = params.subprocesses.max(1) as f64;
    let mut free_at_ms = vec![0.0; members];
    let mut paid: Vec<HashSet<&[String]>> = vec![HashSet::new(); members];
    for shard in shards {
        let member = first_free(&free_at_ms);
        if paid[member].insert(shard.set.as_slice()) {
            free_at_ms[member] += startup_of(startups, &shard.set);
        }

        free_at_ms[member] += shard.predicted_ms / subprocesses + params.round_trip_ms;
    }

    let last_ms = free_at_ms.iter().copied().fold(0.0, f64::max);
    last_ms + params.companion_setup_ms
}

fn first_free(free_at_ms: &[f64]) -> usize {
    free_at_ms
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(member, _)| member)
}

fn startup_of(startups: &HashMap<&[String], f64>, set: &[String]) -> f64 {
    startups.get(set).copied().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_db::eval_costs::{SetCost, SubtreeCost};

    fn segs(path: &[&str]) -> Vec<String> {
        path.iter().map(|seg| seg.to_string()).collect()
    }

    fn params(max_members: usize) -> SplitParams {
        SplitParams {
            max_members,
            subprocesses: 4,
            ..Default::default()
        }
    }

    fn set(path: &[&str], startup_ms: f64) -> SetCost {
        SetCost {
            path: segs(path),
            startup_ms: Some(startup_ms),
            peak_rss_mb: 1024.0,
            max_peak_rss_mb: 1024.0,
        }
    }

    fn subtree(path: &[&str], attrs: f64, ms_per_attr: f64) -> SubtreeCost {
        SubtreeCost {
            path: segs(path),
            attrs,
            ms_per_attr,
        }
    }

    fn flat_flake(set_path: &[&str], startup_ms: f64, children: usize, ms: f64) -> TaskEvalCosts {
        TaskEvalCosts {
            sets: vec![set(set_path, startup_ms)],
            subtrees: (0..children)
                .map(|index| {
                    let mut path = set_path.to_vec();
                    let child = format!("c{index}");
                    path.push(&child);
                    subtree(&path, 1.0, ms)
                })
                .collect(),
        }
    }

    #[test]
    fn no_history_means_one_member() {
        assert_eq!(choose_members(&TaskEvalCosts::default(), &params(4)), 1);
    }

    #[test]
    fn a_small_flake_stays_on_one_worker() {
        let costs = flat_flake(&["hydraJobs"], 2000.0, 3, 100.0);

        assert_eq!(choose_members(&costs, &params(4)), 1);
    }

    #[test]
    fn a_large_flake_uses_every_member() {
        let costs = flat_flake(&["legacyPackages", "x86_64-linux"], 3000.0, 400, 1000.0);

        assert_eq!(choose_members(&costs, &params(4)), 4);
        assert_eq!(choose_members(&costs, &params(2)), 2);
    }

    #[test]
    fn shards_list_the_largest_set_first() {
        let costs = TaskEvalCosts {
            sets: vec![set(&["a"], 0.0), set(&["b"], 0.0)],
            subtrees: vec![
                subtree(&["a", "x"], 5.0, 1000.0),
                subtree(&["b", "y"], 50.0, 1000.0),
            ],
        };
        let listed = listed_from_history(&costs);

        let shards = plan_shards(&costs, &listed, 2, &params(2));

        assert_eq!(shards[0].set, segs(&["b"]));
    }

    #[test]
    fn a_child_without_history_costs_the_median() {
        let costs = TaskEvalCosts {
            sets: vec![set(&["s"], 0.0)],
            subtrees: vec![
                subtree(&["s", "a"], 1.0, 100.0),
                subtree(&["s", "b"], 2.0, 100.0),
                subtree(&["s", "c"], 3.0, 300.0),
            ],
        };
        let listed = [ListedSet {
            path: segs(&["s"]),
            children: segs(&["new"]),
        }];

        let shards = plan_shards(&costs, &listed, 1, &params(1));

        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].predicted_ms, 200.0);
    }

    #[test]
    fn nested_subtrees_count_toward_their_ancestor() {
        let costs = TaskEvalCosts {
            sets: vec![
                set(&["lp", "x86"], 3000.0),
                set(&["lp", "x86", "python3Packages"], 500.0),
            ],
            subtrees: vec![
                subtree(&["lp", "x86", "python3Packages", "numpy"], 1.0, 1000.0),
                subtree(&["lp", "x86", "python3Packages", "requests"], 2.0, 1000.0),
            ],
        };

        let listed = listed_from_history(&costs);
        let shards = plan_shards(&costs, &listed, 1, &params(1));

        assert_eq!(
            listed,
            vec![ListedSet {
                path: segs(&["lp", "x86"]),
                children: segs(&["python3Packages"]),
            }]
        );
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].predicted_ms, 3500.0);
    }
}
