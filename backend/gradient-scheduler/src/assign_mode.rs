/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::BuildSpecKind;

/// A spent miss budget is clearing `cache_available` in the graph writer on the failure that spent
/// it. This function is reading only that flag. Fixed-output `builtin` derivations are
/// `builtin:fetchurl`, and the worker is executing them itself.
pub(crate) fn decide_build_spec_kind(
    cache_available: bool,
    architecture: &str,
    is_fixed_output: bool,
) -> BuildSpecKind {
    if cache_available {
        BuildSpecKind::Substitute
    } else if architecture == gradient_types::BUILTIN_ARCH && is_fixed_output {
        BuildSpecKind::Download
    } else {
        BuildSpecKind::Build
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_available_is_a_substitute_and_everything_else_builds_on_its_arch() {
        assert_eq!(
            decide_build_spec_kind(true, "x86_64-linux", false),
            BuildSpecKind::Substitute
        );
        assert_eq!(
            decide_build_spec_kind(false, "x86_64-linux", false),
            BuildSpecKind::Build
        );
    }

    #[test]
    fn a_builtin_fixed_output_downloads_and_a_builtin_buildenv_builds() {
        assert_eq!(
            decide_build_spec_kind(true, "builtin", true),
            BuildSpecKind::Substitute
        );
        assert_eq!(
            decide_build_spec_kind(false, "builtin", true),
            BuildSpecKind::Download
        );
        assert_eq!(
            decide_build_spec_kind(false, "builtin", false),
            BuildSpecKind::Build
        );
        assert_eq!(
            decide_build_spec_kind(false, "x86_64-linux", true),
            BuildSpecKind::Build
        );
    }
}
