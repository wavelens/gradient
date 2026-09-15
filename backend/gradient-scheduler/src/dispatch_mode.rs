/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BuildDispatchMode {
    RealArch,
    SubstituteBuiltin,
}

/// A relay runs on any worker, since it only moves bytes between two caches;
/// everything else needs a worker of its own architecture.
///
/// Whether an anchor is still worth relaying is not decided here. A spent miss
/// budget clears `substitutable` in the graph actor, on the failure that spends it,
/// so this reads the flag and nothing else - the dispatcher used to escalate a
/// still-substitutable anchor to a real build and then stall it forever when no
/// worker for its architecture was connected.
pub(crate) fn decide_dispatch_mode(substitutable: bool) -> BuildDispatchMode {
    if substitutable {
        BuildDispatchMode::SubstituteBuiltin
    } else {
        BuildDispatchMode::RealArch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutable_is_a_builtin_relay_and_everything_else_needs_its_arch() {
        assert_eq!(
            decide_dispatch_mode(true),
            BuildDispatchMode::SubstituteBuiltin
        );
        assert_eq!(decide_dispatch_mode(false), BuildDispatchMode::RealArch);
    }
}
