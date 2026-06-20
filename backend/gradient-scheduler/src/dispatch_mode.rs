/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BuildDispatchMode {
    RealArch,
    SubstituteBuiltin,
    SubstituteStalled,
}

/// Decide how a ready build should be dispatched.
///
/// - non-substitutable        → `RealArch`
/// - substitutable, under the miss budget → `SubstituteBuiltin` (builtin, any worker)
/// - substitutable, budget spent, a worker for its arch IS connected → `RealArch` (escalate)
/// - substitutable, budget spent, NO worker for its arch → `SubstituteStalled`
pub(crate) fn arch_available(connected: &std::collections::HashSet<String>, arch: &str) -> bool {
    arch == "builtin" || connected.contains(arch)
}

/// Whether an anchor should be substituted rather than built. A fixed-output
/// derivation is content-addressed, so its output is fetchable from any upstream
/// cache regardless of the recorded anchor flag (unless it opts out of
/// substitution); preferring substitution avoids re-running a rotting fetcher.
pub(crate) fn anchor_substitutable(
    anchor_flag: bool,
    is_fixed_output: bool,
    allow_substitutes: bool,
) -> bool {
    anchor_flag || (is_fixed_output && allow_substitutes)
}

pub(crate) fn decide_dispatch_mode(
    substitutable: bool,
    miss_count: i64,
    threshold: i64,
    arch_has_worker: bool,
) -> BuildDispatchMode {
    if !substitutable {
        BuildDispatchMode::RealArch
    } else if miss_count < threshold {
        BuildDispatchMode::SubstituteBuiltin
    } else if arch_has_worker {
        BuildDispatchMode::RealArch
    } else {
        BuildDispatchMode::SubstituteStalled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_substitutable_is_real_arch() {
        assert_eq!(decide_dispatch_mode(false, 0, 2, false), BuildDispatchMode::RealArch);
        assert_eq!(decide_dispatch_mode(false, 5, 2, true), BuildDispatchMode::RealArch);
    }

    #[test]
    fn substitutable_under_threshold_is_builtin() {
        assert_eq!(decide_dispatch_mode(true, 0, 2, false), BuildDispatchMode::SubstituteBuiltin);
        assert_eq!(decide_dispatch_mode(true, 1, 2, false), BuildDispatchMode::SubstituteBuiltin);
    }

    #[test]
    fn escalates_only_when_arch_worker_present() {
        assert_eq!(decide_dispatch_mode(true, 2, 2, true), BuildDispatchMode::RealArch);
    }

    #[test]
    fn stalls_when_budget_spent_and_no_arch_worker() {
        assert_eq!(decide_dispatch_mode(true, 2, 2, false), BuildDispatchMode::SubstituteStalled);
        assert_eq!(decide_dispatch_mode(true, 9, 2, false), BuildDispatchMode::SubstituteStalled);
    }

    #[test]
    fn fods_are_substitutable_even_when_flag_unset() {
        assert!(anchor_substitutable(false, true, true));
        assert!(anchor_substitutable(true, false, false));
    }

    #[test]
    fn non_fods_follow_the_anchor_flag() {
        assert!(!anchor_substitutable(false, false, true));
        assert!(!anchor_substitutable(false, true, false));
    }

    #[test]
    fn arch_available_builtin_always_true() {
        let empty = std::collections::HashSet::new();
        assert!(arch_available(&empty, "builtin"));
        let mut connected = std::collections::HashSet::new();
        connected.insert("x86_64-linux".to_string());
        assert!(arch_available(&connected, "builtin"));
        assert!(arch_available(&connected, "x86_64-linux"));
        assert!(!arch_available(&connected, "aarch64-linux"));
    }
}
