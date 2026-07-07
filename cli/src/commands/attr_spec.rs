/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Flake-output categories that are already gradient attr-path syntax; a head
/// under one of these passes through, anything else is treated as a bare package.
pub const OUTPUT_CATEGORIES: &[&str] = &[
    "packages",
    "legacyPackages",
    "checks",
    "devShells",
    "apps",
    "nixosConfigurations",
    "darwinConfigurations",
    "homeConfigurations",
    "hydraJobs",
    "formatter",
    "bundlers",
];

/// The host's Nix system double (`x86_64-linux`, `aarch64-darwin`, ...), used to
/// qualify a bare `.#uxc` as `packages.<system>.uxc` the way `nix` does.
pub fn default_nix_system() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{}-{}", std::env::consts::ARCH, os)
}

/// Qualify a flake-ref-stripped attr path into gradient's attr-path wildcard
/// language, mirroring `nix`'s default installable resolution: a head that is
/// `*`/`#`/a known output category passes through; a bare package name is
/// prefixed with `packages.<system>.`; an empty attr becomes the whole-package
/// wildcard `packages.<system>.#`. A leading `!` exclusion is preserved.
pub fn qualify_attr(attr: &str, system: &str) -> String {
    let (excl, body) = attr.strip_prefix('!').map(|r| ("!", r)).unwrap_or(("", attr));
    if body.is_empty() {
        return format!("{excl}packages.{system}.#");
    }

    let head = body.split('.').next().unwrap_or("");
    if head == "*" || head == "#" || OUTPUT_CATEGORIES.contains(&head) {
        format!("{excl}{body}")
    } else {
        format!("{excl}packages.{system}.{body}")
    }
}

pub fn parse(spec: &str) -> Result<Vec<String>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty flake reference".to_string());
    }
    let mut out = Vec::new();
    for part in spec.split(',') {
        let attr = part.trim().trim_start_matches('#').trim();
        if attr.is_empty() {
            return Err(format!("empty attribute in '{}'", spec));
        }
        out.push(attr.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_attr_with_hash() {
        assert_eq!(parse("#foo.bar"), Ok(vec!["foo.bar".to_string()]));
    }

    #[test]
    fn parse_single_attr_without_hash() {
        assert_eq!(parse("foo.bar"), Ok(vec!["foo.bar".to_string()]));
    }

    #[test]
    fn parse_multiple_attrs_comma_separated() {
        assert_eq!(
            parse("#a,#b,c"),
            Ok(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn parse_strips_whitespace() {
        assert_eq!(parse(" #a , #b "), Ok(vec!["a".into(), "b".into()]));
    }

    #[test]
    fn parse_rejects_empty_token() {
        assert!(parse("#a,,#b").is_err());
    }

    #[test]
    fn parse_rejects_empty_input() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn qualify_bare_package_gets_packages_prefix() {
        assert_eq!(
            qualify_attr("gradient-cli-full", "x86_64-linux"),
            "packages.x86_64-linux.gradient-cli-full"
        );
    }

    #[test]
    fn qualify_known_category_and_wildcards_pass_through() {
        assert_eq!(
            qualify_attr("packages.aarch64-linux.uxc", "x86_64-linux"),
            "packages.aarch64-linux.uxc"
        );
        assert_eq!(
            qualify_attr("checks.x86_64-linux.*", "x86_64-linux"),
            "checks.x86_64-linux.*"
        );
        assert_eq!(qualify_attr("*", "x86_64-linux"), "*");
        assert_eq!(
            qualify_attr("nixosConfigurations.foo", "x86_64-linux"),
            "nixosConfigurations.foo"
        );
    }

    #[test]
    fn qualify_empty_attr_is_all_packages_and_keeps_exclusion() {
        assert_eq!(qualify_attr("", "x86_64-linux"), "packages.x86_64-linux.#");
        assert_eq!(
            qualify_attr("!uxc", "x86_64-linux"),
            "!packages.x86_64-linux.uxc"
        );
    }
}
