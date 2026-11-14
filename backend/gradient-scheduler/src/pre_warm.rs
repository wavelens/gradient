/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gradient_db::exec_jobs::upcoming::{UpcomingExecJob, without_image};
use gradient_pool::session_port::SessionSignal;
use gradient_pool::{DeviceCount, WorkerCaps, WorkerInfo};
use gradient_types::exec::{GuestSize, WarmGuests, count_warm_guests};
use gradient_util::sync::Mutex;
use tracing::warn;

use crate::Scheduler;
use crate::loops::{caps_of, serves_project};

#[derive(Clone, Default)]
pub(crate) struct PreWarmCounts {
    sent: Arc<Mutex<HashMap<String, Vec<WarmGuests>>>>,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl PreWarmCounts {
    pub(crate) async fn changed(
        &self,
        wanted: BTreeMap<String, Vec<WarmGuests>>,
    ) -> Vec<(String, Vec<WarmGuests>)> {
        let mut sent = self.sent.lock();
        wanted
            .into_iter()
            .filter(|(worker, sizes)| {
                sent.insert(worker.clone(), sizes.clone())
                    .unwrap_or_default()
                    != *sizes
            })
            .collect()
    }

    pub(crate) fn forget(&self, worker_id: &str) {
        self.sent.lock().remove(worker_id);
    }
}

struct UpcomingExecJobForPreWarm {
    fitting_exec_worker_indices: Vec<usize>,
    guest: GuestSize,
    nodes: u32,
}

pub(crate) fn pre_warm_counts(
    upcoming: &[UpcomingExecJob],
    workers: &[WorkerInfo],
) -> BTreeMap<String, Vec<WarmGuests>> {
    let mut exec_workers: Vec<(&WorkerInfo, WorkerCaps)> = workers
        .iter()
        .map(|worker| (worker, caps_of(worker)))
        .filter(|(_, caps)| caps.exec.is_some())
        .collect();
    exec_workers.sort_by(|(a, _), (b, _)| a.id.cmp(&b.id));

    let mut jobs: Vec<UpcomingExecJobForPreWarm> = upcoming
        .iter()
        .filter(|exec| exec.spec.image.is_none())
        .map(|exec| UpcomingExecJobForPreWarm {
            fitting_exec_worker_indices: exec_workers
                .iter()
                .enumerate()
                .filter(|(_, (worker, caps))| {
                    serves_project(worker, exec.project)
                        && caps.exec.as_ref().is_some_and(|inventory| {
                            if inventory.max_server_requested_warm_guests == Some(0) {
                                return false;
                            }
                            let requirement = exec
                                .spec
                                .requirement_on(&exec.architecture, &inventory.defaults);
                            caps.fits_exec(&requirement, DeviceCount::Total)
                        })
                })
                .map(|(index, _)| index)
                .collect(),
            guest: exec.spec.guest,
            nodes: exec.spec.nodes.max(1),
        })
        .filter(|job| !job.fitting_exec_worker_indices.is_empty())
        .collect();
    jobs.sort_by(|a, b| {
        let key = |job: &UpcomingExecJobForPreWarm| {
            (
                job.fitting_exec_worker_indices.len(),
                Reverse(job.nodes),
                job.guest.cpus,
                job.guest.memory_mb,
            )
        };
        key(a).cmp(&key(b)).then_with(|| {
            a.fitting_exec_worker_indices
                .cmp(&b.fitting_exec_worker_indices)
        })
    });

    let reported_max_extra_warm_guests_for_server_count_by_worker: Vec<u32> = exec_workers
        .iter()
        .map(|(_, caps)| {
            caps.exec
                .as_ref()
                .and_then(|inventory| inventory.max_server_requested_warm_guests)
                .unwrap_or(u32::MAX)
        })
        .collect();
    let zones: Vec<Option<&str>> = exec_workers
        .iter()
        .map(|(worker, _)| worker.zone.as_deref())
        .collect();
    let mut warm_vm_count_assigned_to_worker = vec![0_u32; exec_workers.len()];
    let mut assigned: Vec<Vec<(GuestSize, u32)>> = vec![Vec::new(); exec_workers.len()];
    for job in jobs {
        let Some(chosen) = choose_same_zone_workers_for_multinode_job_warm_guests(
            &job,
            &zones,
            &warm_vm_count_assigned_to_worker,
            &reported_max_extra_warm_guests_for_server_count_by_worker,
        ) else {
            continue;
        };
        for index in chosen {
            warm_vm_count_assigned_to_worker[index] += 1;
            assigned[index].push((job.guest, 1));
        }
    }

    exec_workers
        .iter()
        .zip(assigned)
        .map(|((worker, _), sizes)| (worker.id.clone(), count_warm_guests(sizes)))
        .collect()
}

fn choose_same_zone_workers_for_multinode_job_warm_guests(
    job: &UpcomingExecJobForPreWarm,
    zones: &[Option<&str>],
    warm_vm_count_assigned_to_worker: &[u32],
    reported_max_extra_warm_guests_for_server_count_by_worker: &[u32],
) -> Option<Vec<usize>> {
    let preference = |index: usize| {
        let already = warm_vm_count_assigned_to_worker[index];
        (
            already >= reported_max_extra_warm_guests_for_server_count_by_worker[index],
            already,
            index,
        )
    };
    let mut by_zone: BTreeMap<Option<&str>, Vec<usize>> = BTreeMap::new();
    for &index in &job.fitting_exec_worker_indices {
        by_zone.entry(zones[index]).or_default().push(index);
    }
    let nodes = usize::try_from(job.nodes).ok()?;
    by_zone
        .into_values()
        .filter(|members| members.len() >= nodes)
        .map(|mut members| {
            members.sort_by_key(|&index| preference(index));
            members.truncate(nodes);
            members
        })
        .min_by_key(|members| {
            let full = members.iter().filter(|&&index| preference(index).0).count();
            let load: u64 = members
                .iter()
                .map(|&index| u64::from(warm_vm_count_assigned_to_worker[index]))
                .sum();
            (full, load)
        })
}

fn pre_warm_signal(sizes: Vec<WarmGuests>) -> SessionSignal {
    SessionSignal::PreWarmVms {
        count: sizes.iter().map(|warm| warm.count).sum(),
        sizes,
    }
}

impl Scheduler {
    pub(crate) async fn refresh_pre_warm(&self) {
        let _turn = self.pre_warm.turn.lock().await;
        let upcoming = match without_image(&self.db.worker_db).await {
            Ok(upcoming) => upcoming,
            Err(e) => {
                warn!(error = %e, "reading the upcoming exec jobs failed; no pre-warm count sent");
                return;
            }
        };
        let workers = match self.board_workers().await {
            Ok(workers) => workers,
            Err(e) => {
                warn!(error = %e, "Failed to read connected workers. No pre-warm count sent.");
                return;
            }
        };
        let changed = self
            .pre_warm
            .changed(pre_warm_counts(&upcoming, &workers))
            .await;
        if changed.is_empty() {
            return;
        }

        let signals = changed
            .into_iter()
            .map(|(worker, sizes)| (worker, pre_warm_signal(sizes)))
            .collect();
        self.signal_workers(signals).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_jobs::{
        build_only_caps, eight_gpu_exec_caps_with_free, exec_caps_labelled, worker_info,
    };
    use gradient_types::ProjectId;
    use gradient_types::exec::{DeviceRequest, ExecSpec, GuestSize};
    use std::collections::HashSet;

    fn upcoming(spec: ExecSpec) -> UpcomingExecJob {
        upcoming_of(ProjectId::nil(), spec)
    }

    fn upcoming_of(project: ProjectId, spec: ExecSpec) -> UpcomingExecJob {
        UpcomingExecJob {
            spec,
            architecture: "x86_64-linux".into(),
            project,
        }
    }

    fn totals(wanted: BTreeMap<String, Vec<WarmGuests>>) -> BTreeMap<String, u32> {
        wanted
            .into_iter()
            .map(|(worker, sizes)| (worker, sizes.iter().map(|warm| warm.count).sum()))
            .collect()
    }

    fn want(count: u32) -> Vec<WarmGuests> {
        count_warm_guests([(GuestSize::default_request(), count)])
    }

    #[test]
    fn upcoming_jobs_count_per_guest_size() {
        let sized = |cpus| {
            upcoming(ExecSpec {
                guest: GuestSize {
                    cpus,
                    ..GuestSize::default_request()
                },
                ..ExecSpec::default()
            })
        };
        let workers = [worker_info("w1", exec_caps_labelled("lab-a"))];
        let wanted = pre_warm_counts(&[sized(2), sized(8), sized(8), sized(2)], &workers);
        let warm = |cpus| GuestSize {
            cpus,
            ..GuestSize::default_request()
        };
        assert_eq!(
            wanted["w1"],
            count_warm_guests([(warm(2), 2), (warm(8), 2)])
        );
        let SessionSignal::PreWarmVms { count, sizes } = pre_warm_signal(wanted["w1"].clone())
        else {
            panic!("expected a pre-warm signal");
        };
        assert_eq!((count, sizes.len()), (4, 2));
    }

    #[test]
    fn upcoming_two_node_exec_job_prewarms_one_guest_per_worker_in_same_zone_only() {
        let upcoming = [upcoming(ExecSpec {
            nodes: 2,
            labels: vec!["lab-a".into()],
            ..ExecSpec::default()
        })];
        let two_node_worker = |id: &str, label: &str, zone: &str| {
            let mut caps = exec_caps_labelled(label);
            if let Some(inventory) = caps.exec.as_mut() {
                inventory.allow.nodes = 2;
            }
            WorkerInfo {
                zone: Some(zone.into()),
                ..worker_info(id, caps)
            }
        };
        let workers = [
            two_node_worker("w1", "lab-a", "fra1"),
            two_node_worker("w2", "lab-a", "ams1"),
            two_node_worker("w3", "lab-a", "ams1"),
            two_node_worker("w4", "lab-b", "ams1"),
            worker_info("b1", build_only_caps()),
        ];
        assert_eq!(
            totals(pre_warm_counts(&upcoming, &workers)),
            BTreeMap::from([
                ("w1".into(), 0),
                ("w2".into(), 1),
                ("w3".into(), 1),
                ("w4".into(), 0),
            ])
        );
    }

    #[test]
    fn test_pre_warm_sum_over_three_workers_matches_needed_count() {
        let upcoming = [
            upcoming(ExecSpec::default()),
            upcoming(ExecSpec::default()),
            upcoming(ExecSpec::default()),
        ];
        let workers = [
            worker_info("w1", exec_caps_labelled("lab-a")),
            worker_info("w2", exec_caps_labelled("lab-a")),
            worker_info("w3", exec_caps_labelled("lab-a")),
        ];
        assert_eq!(
            totals(pre_warm_counts(&upcoming, &workers)),
            BTreeMap::from([("w1".into(), 1), ("w2".into(), 1), ("w3".into(), 1)])
        );
    }

    #[test]
    fn pre_warm_assignment_skips_worker_with_zero_max_warm_guests_and_prefers_workers_with_room_before_overflow()
     {
        let limited = |id: &str, limit| {
            let mut caps = exec_caps_labelled("lab-a");
            if let Some(inventory) = caps.exec.as_mut() {
                inventory.max_server_requested_warm_guests = Some(limit);
            }
            worker_info(id, caps)
        };
        let upcoming = vec![upcoming(ExecSpec::default()); 4];
        let workers = [limited("w1", 0), limited("w2", 1), limited("w3", 3)];
        assert_eq!(
            totals(pre_warm_counts(&upcoming, &workers)),
            BTreeMap::from([("w1".into(), 0), ("w2".into(), 1), ("w3".into(), 3)])
        );
    }

    #[test]
    fn busy_devices_still_count_and_a_custom_image_counts_nowhere() {
        let gpus = ExecSpec {
            devices: vec![DeviceRequest {
                name: "gpu:h100".into(),
                count: 8,
            }],
            ..ExecSpec::default()
        };
        let image = ExecSpec {
            image: Some("/nix/store/aaaa-toplevel.drv".into()),
            ..ExecSpec::default()
        };
        let workers = [worker_info("w1", eight_gpu_exec_caps_with_free(0))];
        assert_eq!(
            totals(pre_warm_counts(
                &[upcoming(gpus), upcoming(image)],
                &workers
            )),
            BTreeMap::from([("w1".into(), 1)])
        );
    }

    #[test]
    fn a_worker_counts_only_jobs_of_projects_it_serves() {
        let mine = ProjectId::now_v7();
        let other = ProjectId::now_v7();
        let upcoming = [
            upcoming_of(mine, ExecSpec::default()),
            upcoming_of(other, ExecSpec::default()),
        ];
        let restricted = worker_info("restricted", exec_caps_labelled("lab-a"));
        let open = worker_info("open", exec_caps_labelled("lab-a"));
        let draining = worker_info("draining", exec_caps_labelled("lab-a"));
        let workers = [
            WorkerInfo {
                authorized_peers: Some(HashSet::from([mine])),
                ..restricted
            },
            open,
            WorkerInfo {
                draining: true,
                ..draining
            },
        ];
        assert_eq!(
            totals(pre_warm_counts(&upcoming, &workers)),
            BTreeMap::from([
                ("draining".into(), 0),
                ("open".into(), 1),
                ("restricted".into(), 1),
            ])
        );
    }

    #[tokio::test]
    async fn only_changed_counts_are_sent() {
        let counts = PreWarmCounts::default();
        assert_eq!(
            counts
                .changed(BTreeMap::from([
                    ("w1".into(), want(2)),
                    ("w2".into(), want(0))
                ]))
                .await,
            vec![("w1".into(), want(2))]
        );
        assert!(
            counts
                .changed(BTreeMap::from([("w1".into(), want(2))]))
                .await
                .is_empty()
        );
        assert_eq!(
            counts
                .changed(BTreeMap::from([("w1".into(), want(0))]))
                .await,
            vec![("w1".into(), want(0))]
        );
        counts
            .changed(BTreeMap::from([("w1".into(), want(3))]))
            .await;
        counts.forget("w1");
        assert_eq!(
            counts
                .changed(BTreeMap::from([("w1".into(), want(3))]))
                .await,
            vec![("w1".into(), want(3))]
        );
    }
}
