/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use rkyv::{Archive, Deserialize, Serialize};

// ── Scheduling types ──────────────────────────────────────────────────────────

/// A job candidate pushed to workers by the server.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct JobCandidate {
    pub job_id: String,
    /// Store paths the job needs.  Workers check their local store to compute
    /// a substitution score (number of missing paths).
    pub required_paths: Vec<String>,
}

/// A worker's score for a single job candidate.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct CandidateScore {
    pub job_id: String,
    /// Number of `required_paths` not present in the worker's local store.
    /// Lower is better.
    pub missing: u32,
}

// ── Derivation discovery ──────────────────────────────────────────────────────

/// A derivation discovered during `EvaluateDerivations`.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct DiscoveredDerivation {
    /// Attribute path, e.g. `"packages.x86_64-linux.hello"`.
    pub attr: String,
    /// Path to the `.drv` file, e.g. `/nix/store/xxx.drv`.
    pub drv_path: String,
    /// Declared outputs (name → store path).
    pub outputs: Vec<DerivationOutput>,
    /// `.drv` paths this derivation directly depends on.
    pub dependencies: Vec<String>,
    pub architecture: Architecture,
    /// Nix `requiredSystemFeatures` for this derivation.
    pub required_features: Vec<String>,
    /// `true` if all outputs already exist in the worker's local store.
    /// Server marks these as `Substituted` immediately.
    pub substituted: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct DerivationOutput {
    /// Output name, e.g. `"out"`, `"dev"`, `"doc"`.
    pub name: String,
    /// Store path, e.g. `/nix/store/xxx-hello-1.0`.
    pub path: String,
}

/// Build output reported after a derivation successfully builds.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub struct BuildOutput {
    pub name: String,
    pub store_path: String,
    pub hash: String,
    /// NAR size in bytes from `query_pathinfo`.
    pub nar_size: Option<i64>,
    /// NAR hash in SRI format (`sha256-<base64>`).
    pub nar_hash: Option<String>,
    /// `true` if `<output>/nix-support/hydra-build-products` exists.
    pub has_artefacts: bool,
}

// ── Enums ─────────────────────────────────────────────────────────────────────

/// Nix system architecture / platform.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum Architecture {
    /// `builtin:fetchurl` and similar — runs on any platform.
    Builtin,
    X86_64Linux,
    Aarch64Linux,
    X86_64Darwin,
    Aarch64Darwin,
}

/// Type of credential delivered via [`super::server::ServerMessage::Credential`].
#[derive(Archive, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[rkyv(derive(Debug, PartialEq))]
pub enum CredentialKind {
    /// Ed25519 SSH private key for cloning private repositories.
    SshKey,
    /// Ed25519 secret key for signing store paths.
    SigningKey,
}
