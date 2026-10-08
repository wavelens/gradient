/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use gradient_types::ids::ProjectId;

use gradient_wire::types::{BuildStage, GradientCapabilities, JobKind};

use crate::peer_auth::PeerAuth;
use crate::session_port::SessionPort;

mod private {
    pub trait Sealed {}
    impl Sealed for super::Active {}
    impl Sealed for super::Draining {}
}

pub trait WorkerMarker: private::Sealed + std::fmt::Debug + 'static {}

#[derive(Debug)]
pub struct Active;

#[derive(Debug)]
pub struct Draining;

impl WorkerMarker for Active {}
impl WorkerMarker for Draining {}

#[derive(Debug, Clone, PartialEq)]
pub struct AssignedJob {
    pub kind: JobKind,
    pub stage: Option<BuildStage>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkerProfile {
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub cpu_count: u32,
    pub ram_total_mb: u64,
    pub cpu_core_score: u32,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
}

pub struct WorkerShared {
    pub capabilities: GradientCapabilities,
    pub architectures: Vec<String>,
    pub system_features: Vec<String>,
    pub max_concurrent_builds: u32,
    pub cpu_count: u32,
    pub ram_total_mb: u64,
    pub cpu_core_score: u32,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
    pub cpu_usage_pct: Option<f32>,
    pub ram_free_mb: Option<u64>,
    pub disk_speed_mbps: Option<f32>,
    pub upload_speed_mbps: Option<f32>,
    pub download_speed_mbps: Option<f32>,
    pub assigned_jobs: HashMap<String, AssignedJob>,
    pub peer_auth: PeerAuth,
    pub sent_candidates: HashSet<String>,
    pub session: Arc<dyn SessionPort>,
    /// The session loop is bumping this lock-free on every inbound frame. The liveness watchdog is
    /// reading it to detect a worker that died without a clean TCP close.
    pub last_seen: Arc<AtomicI64>,
}

impl WorkerShared {
    pub fn jobs_in(&self, stage: BuildStage) -> u32 {
        self.assigned_jobs
            .values()
            .filter(|job| job.stage == Some(stage))
            .count() as u32
    }

    pub fn profile(&self) -> WorkerProfile {
        WorkerProfile {
            architectures: self.architectures.clone(),
            system_features: self.system_features.clone(),
            max_concurrent_builds: self.max_concurrent_builds,
            cpu_count: self.cpu_count,
            ram_total_mb: self.ram_total_mb,
            cpu_core_score: self.cpu_core_score,
            zone: self.zone.clone(),
            endpoint: self.endpoint.clone(),
        }
    }

    pub fn apply_profile(&mut self, profile: WorkerProfile) {
        self.architectures = profile.architectures;
        self.system_features = profile.system_features;
        self.max_concurrent_builds = profile.max_concurrent_builds;
        self.cpu_count = profile.cpu_count;
        self.ram_total_mb = profile.ram_total_mb;
        self.cpu_core_score = profile.cpu_core_score;
        self.zone = profile.zone.filter(|z| !z.is_empty());
        self.endpoint = profile.endpoint.filter(|e| !e.is_empty());
    }
}

impl std::fmt::Debug for WorkerShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerShared")
            .field("capabilities", &self.capabilities)
            .field("architectures", &self.architectures)
            .field("assigned_jobs", &self.assigned_jobs)
            .field("peer_auth", &self.peer_auth)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct TypedWorker<S: WorkerMarker> {
    pub(crate) shared: WorkerShared,
    _state: PhantomData<S>,
}

impl<S: WorkerMarker> std::ops::Deref for TypedWorker<S> {
    type Target = WorkerShared;
    fn deref(&self) -> &WorkerShared {
        &self.shared
    }
}

impl<S: WorkerMarker> std::ops::DerefMut for TypedWorker<S> {
    fn deref_mut(&mut self) -> &mut WorkerShared {
        &mut self.shared
    }
}

impl TypedWorker<Active> {
    pub fn new(
        capabilities: GradientCapabilities,
        authorized_peers: HashSet<ProjectId>,
        session: Arc<dyn SessionPort>,
    ) -> Self {
        Self {
            shared: WorkerShared {
                capabilities,
                architectures: vec![],
                system_features: vec![],
                max_concurrent_builds: 1,
                cpu_count: 0,
                ram_total_mb: 0,
                cpu_core_score: 0,
                zone: None,
                endpoint: None,
                cpu_usage_pct: None,
                ram_free_mb: None,
                disk_speed_mbps: None,
                upload_speed_mbps: None,
                download_speed_mbps: None,
                assigned_jobs: HashMap::new(),
                peer_auth: PeerAuth::from_peers(authorized_peers),
                sent_candidates: HashSet::new(),
                session,
                last_seen: Arc::new(AtomicI64::new(
                    gradient_types::now().and_utc().timestamp_millis(),
                )),
            },
            _state: PhantomData,
        }
    }

    pub fn has_build_capacity(&self) -> bool {
        let builds = self
            .assigned_jobs
            .values()
            .filter(|job| job.kind == JobKind::Build)
            .count() as u32;
        builds < self.max_concurrent_builds
    }

    pub fn into_draining(self) -> TypedWorker<Draining> {
        TypedWorker {
            shared: self.shared,
            _state: PhantomData,
        }
    }
}
