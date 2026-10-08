/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::codec::Proto;
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Default)]
pub struct GradientCapabilities {
    pub core: bool,
    pub federate: bool,
    pub fetch: bool,
    pub eval: bool,
    pub build: bool,
    pub cache: bool,
}

impl std::ops::BitOrAssign for GradientCapabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        self.core |= rhs.core;
        self.federate |= rhs.federate;
        self.fetch |= rhs.fetch;
        self.eval |= rhs.eval;
        self.build |= rhs.build;
        self.cache |= rhs.cache;
    }
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum Job {
    Flake(FlakeJob),
    Build(BuildJob),
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum FlakeSource {
    Repository { url: String, commit: String },
    Cached { store_path: String },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct FlakeInputOverride {
    pub input_name: String,
    pub url: Option<String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct InputUpdateSpec {
    pub generator: String,
    pub inputs: Vec<String>,
    pub discover_only: bool,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct BumpedInputWire {
    pub name: String,
    pub old_rev: Option<String>,
    pub new_rev: String,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct FlakeJob {
    pub steps: Vec<FlakeStep>,
    pub source: FlakeSource,
    pub wildcards: Vec<String>,
    pub timeout_secs: Option<u64>,
    pub input_overrides: Vec<FlakeInputOverride>,
    pub input_update: Option<InputUpdateSpec>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum FlakeStep {
    FetchFlake,
    EvaluateFlake,
    EvaluateDerivations,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct BuildJob {
    pub builds: Vec<BuildSpec>,
    pub requirement: BuildRequirement,
}

#[derive(Proto, Debug, Clone, PartialEq, Eq, Default)]
pub struct BuildRequirement {
    pub architecture: String,
    pub required_features: Vec<String>,
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildSpecKind {
    #[default]
    Build,
    Substitute,
    Download,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct BuildSpec {
    pub build_id: String,
    pub drv_path: String,
    pub kind: BuildSpecKind,
    pub is_fixed_output: bool,
    pub outputs: Vec<DerivationOutput>,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalMessageLevel {
    Error,
    Warning,
    Notice,
}

pub const ATTR_EVAL_SOURCE_PREFIX: &str = "nix-eval:";

pub fn attr_eval_source(attr: &str) -> String {
    format!("{ATTR_EVAL_SOURCE_PREFIX}{attr}")
}

pub fn attr_of_eval_source(source: &str) -> Option<&str> {
    source.strip_prefix(ATTR_EVAL_SOURCE_PREFIX)
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum JobUpdateKind {
    Fetching,
    FetchResult {
        flake_source: Option<String>,
    },
    EvaluatingFlake,
    EvaluatingDerivations,
    EvalResult {
        derivations: Vec<DiscoveredDerivation>,
        warnings: Vec<String>,
        errors: Vec<String>,
    },
    Building {
        build_id: String,
    },
    BuildOutput {
        build_id: String,
        outputs: Vec<BuildOutput>,
        metrics: Option<BuildMetrics>,
        substituted: bool,
    },
    Compressing,
    EvalStats(EvalStatsReport),
    InputUpdateResult {
        candidate_lock: String,
        bumped: Vec<BumpedInputWire>,
    },
    InputUpdateExpansion {
        matched: Vec<String>,
    },
    Stage(BuildStage),
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildStage {
    Prefetch,
    Build,
    Upload,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct CacheInfo {
    pub file_size: u64,
    pub nar_size: u64,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum EvalCachePullOutcome {
    Miss,
    Presigned {
        url: String,
    },
    Inline {
        total_bytes: u64,
        stream_token: String,
    },
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueryMode {
    #[default]
    Normal,
    Pull,
    Push,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct PresignedMultipart {
    pub upload_id: String,
    pub part_size: u64,
    pub part_urls: Vec<String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct CompletedMultipart {
    pub upload_id: String,
    pub etags: Vec<String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum UploadObject {
    Nar { store_path: String },
    EvalCache { fingerprint: String },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum GrantTarget {
    Skip,
    Passthrough { resume_offset: u64 },
    Put { url: String },
    Multipart(PresignedMultipart),
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct NarUploadMetadata {
    pub file_hash: String,
    pub file_size: u64,
    pub nar_size: u64,
    pub nar_hash: String,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
    pub multipart: Option<CompletedMultipart>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum UploadMetadata {
    Nar(Box<NarUploadMetadata>),
    EvalCache { size_bytes: u64 },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum UploadOutcome {
    Ok,
    Retry { reason: String },
    Rejected { reason: String },
}

#[derive(Proto, Debug, Clone, Default, PartialEq)]
pub struct CachedPath {
    pub path: String,
    pub cached: bool,
    pub file_size: Option<u64>,
    pub nar_size: Option<u64>,
    pub url: Option<String>,
    pub nar_hash: Option<String>,
    pub file_hash: Option<String>,
    pub references: Option<Vec<String>>,
    pub signatures: Option<Vec<String>>,
    pub deriver: Option<String>,
    pub ca: Option<String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct RequiredPath {
    pub path: String,
    pub cache_info: Option<CacheInfo>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct JobCandidate {
    pub job_id: String,
    pub required_paths: Vec<RequiredPath>,
    pub drv_paths: Vec<String>,
    pub output_paths: Vec<String>,
    pub requirement: Option<BuildRequirement>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct CandidateScore {
    pub job_id: String,
    pub missing_count: u32,
    pub missing_nar_size: u64,
    pub outputs_present: bool,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct DiscoveredDerivation {
    pub attr: String,
    pub drv_path: String,
    pub outputs: Vec<DerivationOutput>,
    pub dependencies: Vec<String>,
    pub input_sources: Vec<String>,
    pub architecture: String,
    pub required_features: Vec<String>,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub allow_substitutes: bool,
    pub pname: Option<String>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct DerivationOutput {
    pub name: String,
    pub path: String,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct BuildProduct {
    pub file_type: String,
    pub subtype: String,
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct BuildOutput {
    pub name: String,
    pub store_path: String,
    pub hash: String,
    pub nar_size: Option<i64>,
    pub nar_hash: Option<String>,
    pub products: Vec<BuildProduct>,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct BuildMetrics {
    pub peak_ram_mb: Option<u64>,
    pub cpu_time_ms: Option<u64>,
    pub avg_cpu_pct: Option<f32>,
    pub disk_read_bytes: Option<u64>,
    pub disk_write_bytes: Option<u64>,
    pub oom_killed: bool,
    pub build_time_ms: Option<u64>,
    pub concurrent_builds: Option<u32>,
    pub build_cores: Option<u32>,
    pub cpu_core_score: Option<u32>,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct EvalAttrCost {
    pub attr: String,
    pub thunks: u64,
    pub fn_calls: u64,
    pub eval_ms: u64,
    pub alloc_bytes: u64,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct FlakeOutputNode {
    pub path: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_derivation: bool,
    pub drv_path: Option<String>,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct EvalStatsReport {
    pub total_thunks: u64,
    pub fn_calls: u64,
    pub primop_calls: u64,
    pub lookups: u64,
    pub alloc_bytes: u64,
    pub peak_heap_mb: u64,
    pub peak_rss_mb: u64,
    pub total_eval_ms: u64,
    pub worker_id: String,
    pub per_entry_point: Vec<EvalAttrCost>,
    pub flake_nodes: Vec<FlakeOutputNode>,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum EvalProgress {
    Fetching { inputs: Vec<InputFetch> },
    Evaluating { thunks: u64 },
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct InputFetch {
    pub name: String,
    pub state: InputFetchState,
    pub downloaded_bytes: u64,
    pub expected_bytes: u64,
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFetchState {
    Queued,
    Fetching,
    Done,
    Failed,
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildProgressPhase {
    Download,
    Prefetch,
    Upload,
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobPhase {
    #[default]
    Fetch,
    PushInputs,
    EvalFlake,
    EvalDerivations,
    EvalCachePull,
    EvalCachePush,
    KnownDerivationsWait,
    DrvClosurePush,
    Prefetch,
    SubstituteFetch,
    Download,
    Build,
    Compress,
    NarPush,
    CacheQueryWait,
    NarFetch,
    NarImport,
    UploadWait,
}

impl JobPhase {
    pub const ALL: [Self; 18] = [
        Self::Fetch,
        Self::PushInputs,
        Self::EvalFlake,
        Self::EvalDerivations,
        Self::EvalCachePull,
        Self::EvalCachePush,
        Self::KnownDerivationsWait,
        Self::DrvClosurePush,
        Self::Prefetch,
        Self::SubstituteFetch,
        Self::Download,
        Self::Build,
        Self::Compress,
        Self::NarPush,
        Self::CacheQueryWait,
        Self::NarFetch,
        Self::NarImport,
        Self::UploadWait,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::PushInputs => "push_inputs",
            Self::EvalFlake => "eval_flake",
            Self::EvalDerivations => "eval_derivations",
            Self::EvalCachePull => "eval_cache_pull",
            Self::EvalCachePush => "eval_cache_push",
            Self::KnownDerivationsWait => "known_derivations_wait",
            Self::DrvClosurePush => "drv_closure_push",
            Self::Prefetch => "prefetch",
            Self::SubstituteFetch => "substitute_fetch",
            Self::Download => "download",
            Self::Build => "build",
            Self::Compress => "compress",
            Self::NarPush => "nar_push",
            Self::CacheQueryWait => "cache_query_wait",
            Self::NarFetch => "nar_fetch",
            Self::NarImport => "nar_import",
            Self::UploadWait => "upload_wait",
        }
    }

    /// The discriminant is written out to keep a reordering from re-labelling historical rows.
    /// Value 9 is retired (`substitute_passthrough`) and must stay unused. [`Self::name_of`] is
    /// still naming it for historical spans.
    pub const fn as_i16(self) -> i16 {
        match self {
            Self::Fetch => 0,
            Self::PushInputs => 1,
            Self::EvalFlake => 2,
            Self::EvalDerivations => 3,
            Self::EvalCachePull => 4,
            Self::EvalCachePush => 5,
            Self::KnownDerivationsWait => 6,
            Self::DrvClosurePush => 7,
            Self::Prefetch => 8,
            Self::Build => 10,
            Self::Compress => 11,
            Self::NarPush => 12,
            Self::CacheQueryWait => 13,
            Self::SubstituteFetch => 14,
            Self::Download => 15,
            Self::NarFetch => 16,
            Self::NarImport => 17,
            Self::UploadWait => 18,
        }
    }

    pub const fn from_i16(v: i16) -> Option<Self> {
        Some(match v {
            0 => Self::Fetch,
            1 => Self::PushInputs,
            2 => Self::EvalFlake,
            3 => Self::EvalDerivations,
            4 => Self::EvalCachePull,
            5 => Self::EvalCachePush,
            6 => Self::KnownDerivationsWait,
            7 => Self::DrvClosurePush,
            8 => Self::Prefetch,
            10 => Self::Build,
            11 => Self::Compress,
            12 => Self::NarPush,
            13 => Self::CacheQueryWait,
            14 => Self::SubstituteFetch,
            15 => Self::Download,
            16 => Self::NarFetch,
            17 => Self::NarImport,
            18 => Self::UploadWait,
            _ => return None,
        })
    }

    pub fn name_of(v: i16) -> std::borrow::Cow<'static, str> {
        match (Self::from_i16(v), v) {
            (Some(phase), _) => phase.as_str().into(),
            (None, 9) => "substitute_passthrough".into(),
            (None, _) => format!("unknown_{v}").into(),
        }
    }
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JobPhaseSpan {
    pub phase: JobPhase,
    pub start_ms: u64,
    pub end_ms: u64,
    pub parent: Option<u32>,
    pub paths: u32,
    pub bytes: u64,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum CredentialKind {
    SshKey,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub enum JobKind {
    Flake,
    Build,
}

#[derive(Proto, Debug, Clone, PartialEq, Eq)]
pub struct ClusterAddress {
    pub role: String,
    pub index: u32,
}

#[derive(Proto, Debug, Clone, PartialEq, Eq)]
pub struct ClusterMembership {
    pub attempt: String,
    pub role: String,
    pub index: u32,
    pub hold_secs: u32,
}

#[derive(Proto, Debug, Clone, PartialEq, Eq)]
pub struct ClusterPeer {
    pub role: String,
    pub index: u32,
    pub worker: String,
    pub zone: Option<String>,
    pub endpoint: Option<String>,
}

#[derive(Proto, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BuildFailureKind {
    /// This default is retrying a wire-decode glitch within the bounded attempt budget. A glitch
    /// must not poison the build-once shared build. Unclassified worker errors map to `Permanent`
    /// explicitly in `wire_failure`.
    #[default]
    Transient,
    Permanent,
    Timeout,
    SubstituteUnavailable,
    InputsUnavailable,
    CorruptEvalCache,
    Aborted,
    Canceled,
}

#[cfg(test)]
mod tests {
    use super::JobPhase;

    #[test]
    fn every_stored_phase_discriminant_is_listed() {
        for v in 0..=i16::from(u8::MAX) {
            let listed = JobPhase::ALL.iter().any(|p| p.as_i16() == v);
            assert_eq!(JobPhase::from_i16(v).is_some(), listed, "discriminant {v}");
        }
        for phase in JobPhase::ALL {
            assert_eq!(JobPhase::from_i16(phase.as_i16()), Some(phase));
        }
    }
}
