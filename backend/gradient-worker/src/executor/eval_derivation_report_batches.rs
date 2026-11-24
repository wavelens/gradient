/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::mem;

use gradient_wire::codec;
use gradient_wire::messages::{DiscoveredDerivation, PROTO_VERSIONS};

pub(super) const EVAL_BATCH_SIZE: usize = 500;
pub(super) const EVAL_RESULT_BATCH_MAX_ENCODED_BYTE_SIZE: usize = 1024 * 1024;

pub(super) fn discovered_derivation_encoded_size(derivation: &DiscoveredDerivation) -> usize {
    codec::to_bytes(derivation, *PROTO_VERSIONS.end()).map_or(0, |bytes| bytes.len())
}

#[derive(Default)]
struct ReportBatchCutter {
    full_report_batches_ready_to_send: Vec<Vec<DiscoveredDerivation>>,
    report_batch_being_filled: Vec<DiscoveredDerivation>,
    encoded_byte_size_of_batch_being_filled: usize,
}

impl ReportBatchCutter {
    fn append_record_and_close_batch_if_record_count_or_byte_limit_reached(
        &mut self,
        record: DiscoveredDerivation,
    ) {
        let encoded_record_size = discovered_derivation_encoded_size(&record);
        if !self.report_batch_being_filled.is_empty()
            && self.encoded_byte_size_of_batch_being_filled + encoded_record_size
                > EVAL_RESULT_BATCH_MAX_ENCODED_BYTE_SIZE
        {
            self.move_current_report_batch_to_full_batches();
        }
        self.report_batch_being_filled.push(record);
        self.encoded_byte_size_of_batch_being_filled += encoded_record_size;
        if self.report_batch_being_filled.len() >= EVAL_BATCH_SIZE
            || self.encoded_byte_size_of_batch_being_filled
                >= EVAL_RESULT_BATCH_MAX_ENCODED_BYTE_SIZE
        {
            self.move_current_report_batch_to_full_batches();
        }
    }

    fn move_current_report_batch_to_full_batches(&mut self) {
        self.full_report_batches_ready_to_send
            .push(mem::take(&mut self.report_batch_being_filled));
        self.encoded_byte_size_of_batch_being_filled = 0;
    }

    fn take_full_report_batches(&mut self) -> Vec<Vec<DiscoveredDerivation>> {
        mem::take(&mut self.full_report_batches_ready_to_send)
    }

    fn take_remaining_report_batches(&mut self) -> Vec<Vec<DiscoveredDerivation>> {
        if !self.report_batch_being_filled.is_empty() {
            self.move_current_report_batch_to_full_batches();
        }
        self.take_full_report_batches()
    }
}

struct HeldDerivationWithUnreportedInputCount {
    walked_derivation_record: DiscoveredDerivation,
    unreported_input_derivation_count: usize,
}

/// Scanned derivations stay in the worker until all their input derivations appear in a report, or until those inputs already exist on the server. The server can then inspect each report batch on its own. Batches go out during the scan.
#[derive(Default)]
pub(super) struct DerivationRecordsWaitingForInputs {
    reported_or_stored_derivation_paths: HashSet<String>,
    held_records_by_id: HashMap<usize, HeldDerivationWithUnreportedInputCount>,
    held_record_ids_by_input_derivation_path: HashMap<String, Vec<usize>>,
    next_held_record_id: usize,
    released_record_batch_cutter: ReportBatchCutter,
}

impl DerivationRecordsWaitingForInputs {
    pub(super) fn add_derivation_record_or_hold_until_inputs_reported(
        &mut self,
        record: DiscoveredDerivation,
    ) {
        let unreported_input_derivation_paths: HashSet<String> = record
            .dependencies
            .iter()
            .filter(|input| !self.reported_or_stored_derivation_paths.contains(*input))
            .cloned()
            .collect();
        if unreported_input_derivation_paths.is_empty() {
            return self.append_record_with_all_inputs_reported_and_release_waiting_records(record);
        }

        let held_record_id = self.next_held_record_id;
        self.next_held_record_id += 1;
        let unreported_input_derivation_count = unreported_input_derivation_paths.len();
        for input in unreported_input_derivation_paths {
            self.held_record_ids_by_input_derivation_path
                .entry(input)
                .or_default()
                .push(held_record_id);
        }
        self.held_records_by_id.insert(
            held_record_id,
            HeldDerivationWithUnreportedInputCount {
                walked_derivation_record: record,
                unreported_input_derivation_count,
            },
        );
    }

    pub(super) fn mark_derivation_paths_as_stored(
        &mut self,
        stored_derivation_paths: impl IntoIterator<Item = String>,
    ) {
        for drv_path in stored_derivation_paths {
            self.mark_derivation_reported_and_release_unblocked_derivations_recursively(drv_path);
        }
    }

    pub(super) fn take_full_report_batches(&mut self) -> Vec<Vec<DiscoveredDerivation>> {
        self.released_record_batch_cutter.take_full_report_batches()
    }

    pub(super) fn take_remaining_report_batches(&mut self) -> Vec<Vec<DiscoveredDerivation>> {
        self.held_record_ids_by_input_derivation_path.clear();
        let mut held_records: Vec<(usize, HeldDerivationWithUnreportedInputCount)> =
            self.held_records_by_id.drain().collect();
        held_records.sort_unstable_by_key(|(held_record_id, _)| *held_record_id);
        for (_, held) in held_records {
            self.released_record_batch_cutter
                .append_record_and_close_batch_if_record_count_or_byte_limit_reached(
                    held.walked_derivation_record,
                );
        }
        self.released_record_batch_cutter
            .take_remaining_report_batches()
    }

    fn append_record_with_all_inputs_reported_and_release_waiting_records(
        &mut self,
        record: DiscoveredDerivation,
    ) {
        let drv_path = record.drv_path.clone();
        self.released_record_batch_cutter
            .append_record_and_close_batch_if_record_count_or_byte_limit_reached(record);
        self.mark_derivation_reported_and_release_unblocked_derivations_recursively(drv_path);
    }

    fn mark_derivation_reported_and_release_unblocked_derivations_recursively(
        &mut self,
        drv_path: String,
    ) {
        let mut newly_reported_derivation_paths = vec![drv_path];
        while let Some(drv_path) = newly_reported_derivation_paths.pop() {
            let waiting_record_ids = self
                .held_record_ids_by_input_derivation_path
                .remove(&drv_path)
                .unwrap_or_default();
            self.reported_or_stored_derivation_paths.insert(drv_path);
            for held_record_id in waiting_record_ids {
                let Some(held) = self.held_records_by_id.get_mut(&held_record_id) else {
                    continue;
                };
                held.unreported_input_derivation_count -= 1;
                if held.unreported_input_derivation_count > 0 {
                    continue;
                }
                if let Some(released) = self.held_records_by_id.remove(&held_record_id) {
                    let record = released.walked_derivation_record;
                    newly_reported_derivation_paths.push(record.drv_path.clone());
                    self.released_record_batch_cutter
                        .append_record_and_close_batch_if_record_count_or_byte_limit_reached(
                            record,
                        );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::seq::SliceRandom;
    use rand::{RngExt, SeedableRng};

    fn make_discovered_derivation_with_drv_path_and_input_derivation_paths(
        drv_path: &str,
        inputs: &[&str],
    ) -> DiscoveredDerivation {
        DiscoveredDerivation {
            drv_path: drv_path.to_owned(),
            dependencies: inputs.iter().map(|input| (*input).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn derivation_paths_of_report_batch_records_in_order(
        batches: &[Vec<DiscoveredDerivation>],
    ) -> Vec<&str> {
        batches
            .iter()
            .flatten()
            .map(|record| record.drv_path.as_str())
            .collect()
    }

    #[test]
    fn held_record_goes_to_report_batch_when_its_only_input_is_recorded() {
        let mut waiting = DerivationRecordsWaitingForInputs::default();
        waiting.add_derivation_record_or_hold_until_inputs_reported(
            make_discovered_derivation_with_drv_path_and_input_derivation_paths("app", &["lib"]),
        );
        assert!(waiting.held_records_by_id.contains_key(&0));
        waiting.add_derivation_record_or_hold_until_inputs_reported(
            make_discovered_derivation_with_drv_path_and_input_derivation_paths("lib", &[]),
        );

        assert!(waiting.held_records_by_id.is_empty());
        assert_eq!(
            derivation_paths_of_report_batch_records_in_order(
                &waiting.take_remaining_report_batches()
            ),
            ["lib", "app"]
        );
    }

    #[test]
    fn release_one_input_releases_waiting_chain_in_one_step() {
        let mut waiting = DerivationRecordsWaitingForInputs::default();
        for (drv_path, input) in [("d", "c"), ("c", "b"), ("b", "a"), ("e", "c")] {
            waiting.add_derivation_record_or_hold_until_inputs_reported(
                make_discovered_derivation_with_drv_path_and_input_derivation_paths(
                    drv_path,
                    &[input],
                ),
            );
        }
        assert_eq!(waiting.held_records_by_id.len(), 4);

        waiting.add_derivation_record_or_hold_until_inputs_reported(
            make_discovered_derivation_with_drv_path_and_input_derivation_paths("a", &[]),
        );

        assert!(waiting.held_records_by_id.is_empty());
        let order = waiting.take_remaining_report_batches();
        let order = derivation_paths_of_report_batch_records_in_order(&order);
        let position = |drv_path| order.iter().position(|path| *path == drv_path).unwrap();
        assert_eq!(order.len(), 5);
        assert!(position("a") < position("b"));
        assert!(position("b") < position("c"));
        assert!(position("c") < position("d"));
        assert!(position("c") < position("e"));
    }

    #[test]
    fn known_input_does_not_block_dependent_record() {
        let mut waiting = DerivationRecordsWaitingForInputs::default();
        waiting.add_derivation_record_or_hold_until_inputs_reported(
            make_discovered_derivation_with_drv_path_and_input_derivation_paths(
                "app",
                &["stored-before", "stored-after"],
            ),
        );
        waiting.mark_derivation_paths_as_stored(["stored-before".to_owned()]);
        assert_eq!(waiting.held_records_by_id.len(), 1);
        waiting.mark_derivation_paths_as_stored(["stored-after".to_owned()]);
        assert!(waiting.held_records_by_id.is_empty());

        waiting.add_derivation_record_or_hold_until_inputs_reported(
            make_discovered_derivation_with_drv_path_and_input_derivation_paths(
                "tool",
                &["stored-before"],
            ),
        );
        assert!(waiting.held_records_by_id.is_empty());
        assert_eq!(
            derivation_paths_of_report_batch_records_in_order(
                &waiting.take_remaining_report_batches()
            ),
            ["app", "tool"]
        );
    }

    #[test]
    fn records_not_filling_a_batch_are_returned_only_at_the_end_of_the_walk() {
        let mut waiting = DerivationRecordsWaitingForInputs::default();
        for drv_path in ["a", "b"] {
            waiting.add_derivation_record_or_hold_until_inputs_reported(
                make_discovered_derivation_with_drv_path_and_input_derivation_paths(drv_path, &[]),
            );
        }
        waiting.add_derivation_record_or_hold_until_inputs_reported(
            make_discovered_derivation_with_drv_path_and_input_derivation_paths(
                "cycle",
                &["cycle"],
            ),
        );

        assert!(waiting.take_full_report_batches().is_empty());
        let remaining = waiting.take_remaining_report_batches();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            derivation_paths_of_report_batch_records_in_order(&remaining),
            ["a", "b", "cycle"]
        );
        assert!(waiting.take_remaining_report_batches().is_empty());
    }

    #[test]
    fn batch_is_full_and_sent_at_500_records() {
        let mut waiting = DerivationRecordsWaitingForInputs::default();
        let drv_paths: Vec<String> = (0..EVAL_BATCH_SIZE * 2 + 1)
            .map(|n| format!("n{n}"))
            .collect();
        for drv_path in &drv_paths[..EVAL_BATCH_SIZE - 1] {
            waiting.add_derivation_record_or_hold_until_inputs_reported(
                make_discovered_derivation_with_drv_path_and_input_derivation_paths(drv_path, &[]),
            );
        }
        assert!(waiting.take_full_report_batches().is_empty());

        for drv_path in &drv_paths[EVAL_BATCH_SIZE - 1..] {
            waiting.add_derivation_record_or_hold_until_inputs_reported(
                make_discovered_derivation_with_drv_path_and_input_derivation_paths(drv_path, &[]),
            );
        }
        let full: Vec<usize> = waiting
            .take_full_report_batches()
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(full, [EVAL_BATCH_SIZE, EVAL_BATCH_SIZE]);
        let remaining: Vec<usize> = waiting
            .take_remaining_report_batches()
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(remaining, [1]);
    }

    #[test]
    fn every_input_is_known_or_in_same_or_earlier_batch() {
        for seed in 0..20 {
            let mut random = rand::rngs::StdRng::seed_from_u64(seed);
            let derivation_count = 3000;
            let inputs: Vec<Vec<String>> = (0..derivation_count)
                .map(|n: usize| {
                    let input_count = if n == 0 { 0 } else { random.random_range(0..6) };
                    let mut inputs: Vec<String> = (0..input_count)
                        .map(|_| format!("n{}", random.random_range(0..n)))
                        .collect();
                    inputs.sort_unstable();
                    inputs.dedup();
                    inputs
                })
                .collect();
            let stored: HashSet<String> = (0..derivation_count)
                .filter(|_| random.random_bool(0.1))
                .map(|n| format!("n{n}"))
                .collect();
            let mut walk_order: Vec<usize> = (0..derivation_count)
                .filter(|n| !stored.contains(&format!("n{n}")))
                .collect();
            walk_order.shuffle(&mut random);
            let mut stored_answer_order: Vec<String> = stored.iter().cloned().collect();
            stored_answer_order.shuffle(&mut random);

            let mut waiting = DerivationRecordsWaitingForInputs::default();
            let mut batches = Vec::new();
            let mut stored_answers = stored_answer_order.into_iter();
            for n in &walk_order {
                if random.random_bool(0.1) {
                    waiting.mark_derivation_paths_as_stored(stored_answers.by_ref().take(5));
                }
                let input_paths: Vec<&str> = inputs[*n].iter().map(String::as_str).collect();
                waiting.add_derivation_record_or_hold_until_inputs_reported(
                    make_discovered_derivation_with_drv_path_and_input_derivation_paths(
                        &format!("n{n}"),
                        &input_paths,
                    ),
                );
                batches.extend(waiting.take_full_report_batches());
            }
            waiting.mark_derivation_paths_as_stored(stored_answers);
            batches.extend(waiting.take_full_report_batches());
            let streamed_batch_count = batches.len();
            batches.extend(waiting.take_remaining_report_batches());

            assert!(streamed_batch_count >= 4, "seed {seed}");
            assert!(
                batches
                    .iter()
                    .all(|batch| !batch.is_empty() && batch.len() <= EVAL_BATCH_SIZE)
            );
            let mut reported: HashSet<&str> = HashSet::new();
            for batch in &batches {
                let in_batch: HashSet<&str> = batch
                    .iter()
                    .map(|record| record.drv_path.as_str())
                    .collect();
                for record in batch {
                    for input in &record.dependencies {
                        assert!(
                            stored.contains(input)
                                || reported.contains(input.as_str())
                                || in_batch.contains(input.as_str()),
                            "seed {seed}: {input} of {}",
                            record.drv_path
                        );
                    }
                }
                reported.extend(in_batch);
            }
            assert_eq!(reported.len(), walk_order.len(), "seed {seed}");
        }
    }
}
