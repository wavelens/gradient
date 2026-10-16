/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use chrono::NaiveDateTime;
use gradient_pool::estimate::{GroupKey, Sample, fewer_cores};
use gradient_types::ids::DerivationId;
use uuid::Uuid;

pub(crate) struct LoadedSample {
    pub id: Uuid,
    pub created_at: NaiveDateTime,
    pub oom_killed: bool,
    pub sample: Sample,
}

#[derive(Default)]
pub(crate) struct SampleStore {
    seen: HashSet<Uuid>,
    rows: Vec<LoadedSample>,
    groups: HashMap<GroupKey, Vec<usize>>,
    oom_cores: HashMap<DerivationId, u32>,
    pub newest: Option<NaiveDateTime>,
}

impl SampleStore {
    pub fn add(&mut self, loaded: Vec<LoadedSample>) {
        for row in loaded {
            if !self.seen.insert(row.id) {
                continue;
            }
            self.newest = self.newest.max(Some(row.created_at));
            if row.oom_killed
                && let Some(cores) = row.sample.cores
            {
                let fewest = self.oom_cores.entry(row.sample.derivation).or_insert(cores);
                *fewest = (*fewest).min(cores);
            }
            self.groups
                .entry(row.sample.group())
                .or_default()
                .push(self.rows.len());
            self.rows.push(row);
        }
    }

    pub fn samples(&self) -> Vec<Sample> {
        self.rows.iter().map(|r| r.sample.clone()).collect()
    }

    pub fn own(&self, key: &GroupKey) -> Vec<&Sample> {
        self.groups
            .get(key)
            .into_iter()
            .flatten()
            .map(|&i| &self.rows[i].sample)
            .collect()
    }

    pub fn max_cores(&self, derivation: DerivationId) -> Option<u32> {
        self.oom_cores
            .get(&derivation)
            .map(|&cores| fewer_cores(cores))
    }

    pub fn prune_before(&mut self, oldest: NaiveDateTime) {
        let newest = self.newest;
        let kept: Vec<LoadedSample> = std::mem::take(&mut self.rows)
            .into_iter()
            .filter(|r| r.created_at >= oldest)
            .collect();
        *self = Self::default();
        self.add(kept);
        self.newest = newest;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: Uuid, derivation: DerivationId, cores: u32, oom: bool) -> LoadedSample {
        LoadedSample {
            id,
            created_at: gradient_types::now(),
            oom_killed: oom,
            sample: Sample {
                derivation,
                architecture: "x86_64-linux".into(),
                pname: "torch".into(),
                input_pnames: vec!["cmake".into()],
                cores: Some(cores),
                peak_ram_mb: Some(100_000),
                build_ms: (!oom).then_some(1.0),
                output_nar_bytes: None,
                lower_bound: oom,
            },
        }
    }

    #[test]
    fn a_row_read_twice_counts_once() {
        let mut store = SampleStore::default();
        let id = Uuid::now_v7();
        let derivation = DerivationId::now_v7();
        store.add(vec![row(id, derivation, 64, false)]);
        store.add(vec![row(id, derivation, 64, false)]);
        assert_eq!(store.samples().len(), 1);
    }

    #[test]
    fn the_core_cap_follows_the_fewest_cores_a_derivation_ran_out_of_memory_with() {
        let mut store = SampleStore::default();
        let derivation = DerivationId::now_v7();
        store.add(vec![
            row(Uuid::now_v7(), derivation, 64, true),
            row(Uuid::now_v7(), derivation, 43, true),
        ]);
        assert_eq!(store.max_cores(derivation), Some(29));
        assert_eq!(store.max_cores(DerivationId::now_v7()), None);
    }

    #[test]
    fn a_row_without_a_build_time_that_was_not_killed_caps_nothing() {
        let mut store = SampleStore::default();
        let derivation = DerivationId::now_v7();
        let mut lost = row(Uuid::now_v7(), derivation, 64, false);
        lost.sample.build_ms = None;
        lost.sample.lower_bound = true;
        store.add(vec![lost]);

        assert_eq!(store.max_cores(derivation), None);
    }
}
