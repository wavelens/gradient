/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use chrono::NaiveDateTime;
use gradient_pool::estimate::{GroupKey, Sample, fewer_cores, input_overlap};
use gradient_pool::score::weights::{MIN_INPUT_OVERLAP, OWN_SAMPLE_WINDOW};
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
    by_pname: HashMap<(String, String), Vec<usize>>,
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
            self.by_pname
                .entry((row.sample.architecture.clone(), row.sample.pname.clone()))
                .or_default()
                .push(self.rows.len());
            self.rows.push(row);
        }
    }

    pub fn samples(&self) -> Vec<Sample> {
        self.rows.iter().map(|r| r.sample.clone()).collect()
    }

    pub fn own(&self, key: &GroupKey) -> Vec<(f64, &Sample)> {
        let Some(rows) = self
            .by_pname
            .get(&(key.architecture.clone(), key.pname.clone()))
        else {
            return Vec::new();
        };
        rows.iter()
            .rev()
            .take(OWN_SAMPLE_WINDOW)
            .map(|&i| &self.rows[i].sample)
            .map(|sample| {
                (
                    input_overlap(&key.input_pnames, &sample.input_pnames),
                    sample,
                )
            })
            .filter(|(overlap, _)| *overlap >= MIN_INPUT_OVERLAP)
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

    fn with_inputs(inputs: &[&str]) -> LoadedSample {
        let mut sample = row(Uuid::now_v7(), DerivationId::now_v7(), 16, false);
        sample.sample.input_pnames = inputs.iter().map(|i| (*i).to_owned()).collect();
        sample
    }

    fn key(inputs: &[&str]) -> GroupKey {
        with_inputs(inputs).sample.group()
    }

    #[test]
    fn a_build_with_other_inputs_under_the_same_pname_is_not_an_own_build() {
        let mut store = SampleStore::default();
        store.add(vec![with_inputs(&["cmake", "cudatoolkit"])]);

        assert!(
            store
                .own(&key(&["autoPatchelfHook", "python3-wheel"]))
                .is_empty()
        );
    }

    #[test]
    fn an_own_build_counts_by_the_share_of_inputs_both_have() {
        let mut store = SampleStore::default();
        store.add(vec![with_inputs(&["a", "b", "c", "d"])]);

        let own = store.own(&key(&["a", "b", "c", "e"]));
        assert_eq!(own.len(), 1);
        assert!((own[0].0 - 0.6).abs() < 1e-9, "{}", own[0].0);
    }
}
