/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Failure classification - the one place an error becomes a
//! [`BuildFailureKind`] on its way to the server.

use gradient_proto::messages::BuildFailureKind;

use crate::executor::eval::CorruptEvalCache;
use crate::proto::prefetch::{CorruptCachedNar, MissingInputs, SubstituteNotOnUpstream};

// ── BuildError ────────────────────────────────────────────────────────────────

/// A build failure carrying its classification, so the dispatch layer can
/// report the right `BuildFailureKind` to the server.
#[derive(Debug)]
pub struct BuildError {
    pub kind: BuildFailureKind,
    pub source: anyhow::Error,
    /// For `BuildFailureKind::InputsUnavailable`: the required input store paths
    /// the cache could not serve. Empty for every other kind.
    pub missing_paths: Vec<String>,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.source)
    }
}
impl std::error::Error for BuildError {}

impl BuildError {
    pub(super) fn new(kind: BuildFailureKind, source: anyhow::Error) -> Self {
        Self {
            kind,
            source,
            missing_paths: Vec::new(),
        }
    }
    pub(crate) fn transient(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::Transient, e.into())
    }
    pub(crate) fn permanent(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::Permanent, e.into())
    }
    pub(crate) fn timeout(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::Timeout, e.into())
    }
    /// A substitute attempt missed: this worker could not pull the output from
    /// cache. Never falls back to a local build (wrong-arch); the scheduler
    /// re-dispatches or escalates to a real build.
    pub(crate) fn substitute_unavailable(e: impl Into<anyhow::Error>) -> Self {
        Self::new(BuildFailureKind::SubstituteUnavailable, e.into())
    }
    /// Prefetch found required inputs the gradient cache cannot serve. Carries
    /// the offending paths so the server demotes them and re-queues their
    /// producers; terminal for this build.
    pub(crate) fn inputs_unavailable(
        missing_paths: Vec<String>,
        e: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            kind: BuildFailureKind::InputsUnavailable,
            source: e.into(),
            missing_paths,
        }
    }
    /// The server sent `AbortJob` while the daemon was building. Reported as
    /// its own kind, never `Permanent`: a `Permanent` build failure is recorded
    /// as `BuilderNonzero`, which permanently excludes the anchor from every
    /// requeue even though `Aborted` is a requeueable status (#572).
    pub(crate) fn aborted(drv_path: &str) -> Self {
        Self::new(
            BuildFailureKind::Aborted,
            JobAborted(format!("build aborted by server: {drv_path}")).into(),
        )
    }
}

// ── JobAborted ────────────────────────────────────────────────────────────────

/// The job stopped because the server ordered it to. Carried as a typed error so
/// [`wire_failure`] classifies it wherever the abort is noticed - inside the
/// daemon log drain, at a NAR-push checkpoint, or between eval waves - instead of
/// falling through to the unclassified-`Permanent` branch.
#[derive(Debug)]
pub struct JobAborted(pub String);

impl std::fmt::Display for JobAborted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for JobAborted {}

// ── Builder-message classification ────────────────────────────────────────────

/// Best-effort OOM signature scan. OOM presents as a generic build failure but
/// is transient (retry on a less-loaded builder).
pub(super) fn looks_like_oom(msg: &str) -> bool {
    let l = msg.to_ascii_lowercase();
    l.contains("out of memory")
        || l.contains("cannot allocate memory")
        || l.contains("oom-killer")
        || l.contains("killed")
}

/// Signatures of a failure in the store or the daemon rather than in the
/// derivation. These say nothing about whether the build *would* succeed, so
/// treating them as deterministic strands the build: `Permanent` is never
/// re-thawed, and every dependent cascades to `DependencyFailed`.
const INFRA_FAILURE_SIGNATURES: &[&str] = &[
    "is not valid",
    "does not exist in the store",
    "no space left on device",
    "cannot connect to daemon",
    "unexpected end-of-file",
    "input/output error",
    "connection reset by peer",
    "broken pipe",
];

/// Best-effort scan for a store/daemon failure. Matched on the raw message, so
/// the ANSI escapes nix wraps its errors in cannot hide a signature.
pub(super) fn looks_like_infra_failure(msg: &str) -> bool {
    let l = msg.to_ascii_lowercase();
    INFRA_FAILURE_SIGNATURES.iter().any(|s| l.contains(s))
}

/// Classify a builder-reported failure message: OOM or an infrastructure fault
/// -> Transient, otherwise a real build error -> Permanent.
pub(super) fn classify_build_error(msg: &str) -> BuildFailureKind {
    if looks_like_oom(msg) || looks_like_infra_failure(msg) {
        BuildFailureKind::Transient
    } else {
        BuildFailureKind::Permanent
    }
}

// ── Transfer-error classification ─────────────────────────────────────────────

/// Classify an input-prefetch failure.
///
/// A "required inputs not in cache" miss is terminal and self-healing
/// server-side: forward the paths so the server demotes them and re-queues
/// their producers. A cached NAR that fails integrity (its bytes don't match
/// the recorded nar_hash, e.g. a non-reproducible local build desynced from
/// upstream-substitute metadata) is the same class: report the path so the
/// server demotes the corrupt object and rebuilds it. Every other prefetch
/// error is infrastructure-transient.
pub(super) fn classify_prefetch_error(build_id: &str, e: anyhow::Error) -> BuildError {
    tracing::error!(%build_id, error = %e, "input prefetch failed; aborting build");
    if let Some(mi) = e.downcast_ref::<MissingInputs>() {
        BuildError::inputs_unavailable(mi.0.clone(), e)
    } else if let Some(corrupt) = e.chain().find_map(|s| s.downcast_ref::<CorruptCachedNar>()) {
        BuildError::inputs_unavailable(vec![corrupt.0.clone()], e)
    } else {
        BuildError::transient(e)
    }
}

/// Classify an `external_cached` substitute-relay failure.
pub(super) fn classify_substitute_failure(build_id: &str, e: anyhow::Error) -> BuildError {
    if e.chain().any(|c| c.is::<SubstituteNotOnUpstream>()) {
        tracing::warn!(%build_id, error = %e, "external_cached relay: output on no upstream; SubstituteUnavailable");
        BuildError::substitute_unavailable(e)
    } else if let Some(mi) = e.chain().find_map(|c| c.downcast_ref::<MissingInputs>()) {
        // The upstream advertised the path but the object GET 404'd: surface
        // the paths so the server's demote/reconcile self-heal clears the
        // stale record instead of this build retrying against it forever.
        tracing::warn!(%build_id, error = %e, "external_cached relay: advertised NAR object missing; InputsUnavailable");
        BuildError::inputs_unavailable(mi.0.clone(), e)
    } else {
        tracing::warn!(%build_id, error = %e, "external_cached relay failed transiently; retrying without escalating");
        BuildError::transient(e)
    }
}

// ── Wire mapping ──────────────────────────────────────────────────────────────

/// Map a finished job's error to the `(kind, missing_paths)` pair reported in
/// `ClientMessage::JobFailed`. Anything that isn't a [`BuildError`] (eval-job
/// failures, plumbing errors) is an explicit, logged Permanent fallthrough -
/// never a silent default.
pub(crate) fn wire_failure(e: &anyhow::Error) -> (BuildFailureKind, Vec<String>) {
    // Eval-job corruption: carry the blob fingerprint so the server purges it.
    if let Some(c) = e.chain().find_map(|s| s.downcast_ref::<CorruptEvalCache>()) {
        return (
            BuildFailureKind::CorruptEvalCache,
            vec![c.fingerprint.clone()],
        );
    }
    // An abort noticed outside the build itself (NAR push checkpoints, eval wave
    // boundaries) arrives as a bare `JobAborted`, not wrapped in a `BuildError`.
    if e.chain().any(|s| s.is::<JobAborted>()) {
        return (BuildFailureKind::Aborted, Vec::new());
    }
    match e.downcast_ref::<BuildError>() {
        Some(be) => (be.kind, be.missing_paths.clone()),
        None => {
            tracing::warn!(error = %format!("{e:#}"), "unclassified job error reported as Permanent");
            (BuildFailureKind::Permanent, Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_and_looks_like_oom() {
        assert_eq!(
            classify_build_error("gcc: fatal error: Killed signal terminated program cc1plus"),
            BuildFailureKind::Transient
        );
        assert_eq!(
            classify_build_error("error: undefined reference to `foo'"),
            BuildFailureKind::Permanent
        );
        assert!(looks_like_oom("Cannot allocate memory"));
        assert!(looks_like_oom("Killed"));
        assert!(looks_like_oom("oom-killer: invoked"));
        assert!(!looks_like_oom("error: undefined reference to `foo'"));
    }

    /// The failure that took down eval `019fcf38`: a single missing input path
    /// was classified `Permanent`, so it was never retried and cascaded into
    /// 2,687 `DependencyFailed` dependents. A store path the daemon refuses is
    /// infrastructure, never a deterministic property of the derivation.
    #[test]
    fn a_store_or_daemon_error_is_transient_not_permanent() {
        for msg in [
            "error: path '/nix/store/p59cz-coreutils-9.11.tar.xz' is not valid",
            "error: path '/nix/store/abc-foo' does not exist in the store",
            "error: cannot connect to daemon at '/nix/var/nix/daemon-socket/socket'",
            "error: writing to file: No space left on device",
            "error: unexpected end-of-file",
            "error: reading from file: Input/output error",
            "error: connection reset by peer",
        ] {
            assert_eq!(
                classify_build_error(msg),
                BuildFailureKind::Transient,
                "infrastructure failure must stay retryable: {msg}"
            );
        }
    }

    /// The escape codes nix wraps its messages in must not hide a signature.
    #[test]
    fn ansi_coloured_daemon_errors_are_still_recognised() {
        let coloured = "build failed: \u{1b}[31;1merror:\u{1b}[0m path \
             '\u{1b}[35;1m/nix/store/p59cz-coreutils-9.11.tar.xz\u{1b}[0m' is not valid";
        assert_eq!(classify_build_error(coloured), BuildFailureKind::Transient);
    }

    /// A real compile failure stays terminal: misrouting these to `Transient`
    /// would retry every broken derivation until its attempt budget ran out.
    #[test]
    fn genuine_build_errors_stay_permanent() {
        for msg in [
            "error: undefined reference to `foo'",
            "error: test suite failed with exit code 1",
            "make: *** [Makefile:42: all] Error 2",
        ] {
            assert_eq!(
                classify_build_error(msg),
                BuildFailureKind::Permanent,
                "deterministic build failure must not be retried: {msg}"
            );
        }
    }

    #[test]
    fn wire_failure_downcasts_build_error() {
        let be = BuildError::inputs_unavailable(
            vec!["/nix/store/x-y".into()],
            anyhow::anyhow!("missing"),
        );
        let e: anyhow::Error = be.into();
        let (kind, missing) = wire_failure(&e);
        assert_eq!(kind, BuildFailureKind::InputsUnavailable);
        assert_eq!(missing, vec!["/nix/store/x-y".to_owned()]);
    }

    #[test]
    fn wire_failure_maps_corrupt_eval_cache_with_fingerprint() {
        let e = anyhow::Error::new(CorruptEvalCache {
            fingerprint: "abc123".into(),
        })
        .context("evaluate flake");
        let (kind, missing) = wire_failure(&e);
        assert_eq!(kind, BuildFailureKind::CorruptEvalCache);
        assert_eq!(
            missing,
            vec!["abc123".to_owned()],
            "the fingerprint rides missing_paths so the server can purge the blob"
        );
    }

    /// An abort must never reach the server as `Permanent`: that is stored as
    /// `BuilderNonzero` and permanently blocks the anchor from being requeued
    /// (#572). Both shapes are covered - the `BuildError` raised inside the
    /// daemon log drain, and the bare `JobAborted` raised at a NAR-push
    /// checkpoint or an eval wave boundary.
    #[test]
    fn an_abort_is_reported_as_aborted_not_permanent() {
        let from_build: anyhow::Error = BuildError::aborted("/nix/store/x.drv").into();
        assert_eq!(wire_failure(&from_build).0, BuildFailureKind::Aborted);

        let from_checkpoint = anyhow::Error::new(JobAborted("job aborted by server".into()))
            .context("compress and push NARs");
        assert_eq!(wire_failure(&from_checkpoint).0, BuildFailureKind::Aborted);
    }

    #[test]
    fn wire_failure_unclassified_is_explicit_permanent() {
        let e = anyhow::anyhow!("some plumbing exploded");
        let (kind, missing) = wire_failure(&e);
        assert_eq!(kind, BuildFailureKind::Permanent);
        assert!(missing.is_empty());
    }

    #[test]
    fn prefetch_missing_inputs_carries_paths() {
        let e = anyhow::Error::new(crate::proto::prefetch::MissingInputs(vec![
            "/nix/store/a-b".into(),
        ]));
        let be = classify_prefetch_error("b1", e);
        assert_eq!(be.kind, BuildFailureKind::InputsUnavailable);
        assert_eq!(be.missing_paths, vec!["/nix/store/a-b".to_owned()]);
    }

    #[test]
    fn substitute_relay_404_is_inputs_unavailable() {
        let e = anyhow::Error::new(crate::proto::prefetch::MissingInputs(vec![
            "/nix/store/a-b".into(),
        ]))
        .context("download upstream NAR");
        let be = classify_substitute_failure("b1", e);
        assert_eq!(be.kind, BuildFailureKind::InputsUnavailable);
        assert_eq!(be.missing_paths, vec!["/nix/store/a-b".to_owned()]);
    }

    /// Only a genuine "not on any upstream" miss escalates; a transient relay
    /// timeout (Pull RPC / NAR download / presigned PUT) retries as a substitute
    /// instead of counting toward miss-escalation - two transient timeouts must
    /// not turn a substitutable build into a from-scratch one.
    #[test]
    fn substitute_wrapped_and_transient_classification() {
        use crate::proto::prefetch::SubstituteNotOnUpstream;

        let wrapped = classify_substitute_failure(
            "b",
            anyhow::Error::new(SubstituteNotOnUpstream("/nix/store/p".into())).context("relay"),
        );
        assert!(matches!(
            wrapped.kind,
            BuildFailureKind::SubstituteUnavailable
        ));

        let timeout = classify_substitute_failure("b", anyhow::anyhow!("operation timed out"));
        assert!(matches!(timeout.kind, BuildFailureKind::Transient));
    }

    #[test]
    fn substitute_not_on_upstream_wins() {
        let e = anyhow::Error::new(crate::proto::prefetch::SubstituteNotOnUpstream(
            "/nix/store/a-b".into(),
        ));
        let be = classify_substitute_failure("b1", e);
        assert_eq!(be.kind, BuildFailureKind::SubstituteUnavailable);
    }
}
