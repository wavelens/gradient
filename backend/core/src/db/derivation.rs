/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Parser for Nix `.drv` (ATerm derivation) files.
//!
//! A derivation file has the form:
//! ```text
//! Derive([outputs],[inputDrvs],[inputSrcs],"system","builder",[args],[("KEY","VALUE"),...])
//! ```

use anyhow::{Result, anyhow};
use std::collections::HashMap;

/// A single output of a derivation, e.g. `("out", "/nix/store/hash-foo", "", "")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivationOutput {
    pub name: String,
    /// Store path for this output, or empty for content-addressed derivations.
    pub path: String,
    /// Hash algorithm (e.g. `"sha256"`), or empty.
    pub hash_algo: String,
    /// Hash value, or empty.
    pub hash: String,
}

/// A fully parsed Nix derivation.
#[derive(Debug, Clone)]
pub struct Derivation {
    pub outputs: Vec<DerivationOutput>,
    /// Map of `.drv` path → set of output names required from it.
    pub input_derivations: Vec<InputDrv>,
    /// Plain store paths (not derivations) needed at build time.
    pub input_sources: Vec<String>,
    pub system: String,
    pub builder: String,
    pub args: Vec<String>,
    pub environment: HashMap<String, String>,
}

/// Build-relevant attributes extracted from a derivation's environment.
///
/// Generalizes the old `required_system_features` accessor. `meta.*` Nix
/// attributes do not survive into the `.drv`; these are read from top-level
/// derivation attributes that *do* land in `drv.environment`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildMeta {
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub prefer_local_build: bool,
    pub required_features: Vec<String>,
}

impl Derivation {
    /// Returns the `requiredSystemFeatures` as a list. The env var is stored as a
    /// space-separated string inside the derivation.
    pub fn required_system_features(&self) -> Vec<String> {
        self.environment
            .get("requiredSystemFeatures")
            .map(|v| {
                v.split_whitespace()
                    .filter(|f| !f.is_empty())
                    .map(|f| f.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the derivation permits substitution from a binary cache.
    /// Nix defaults to `true`; a present attr disables it unless it reads as
    /// truthy (`"1"`/`"true"`). Nix serializes `allowSubstitutes = false` as
    /// `""` in the env, so an empty or `"0"`/`"false"` value means disabled.
    pub fn allow_substitutes(&self) -> bool {
        self.environment
            .get("allowSubstitutes")
            .map(|v| matches!(v.trim(), "1" | "true"))
            .unwrap_or(true)
    }

    /// Extract all build-relevant attributes in one pass.
    pub fn build_meta(&self) -> BuildMeta {
        let secs = |key: &str| {
            self.environment
                .get(key)
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        let prefer_local_build = self
            .environment
            .get("preferLocalBuild")
            .map(|v| matches!(v.trim(), "1" | "true"))
            .unwrap_or(false);
        BuildMeta {
            timeout_secs: secs("timeout"),
            max_silent_secs: secs("maxSilent"),
            prefer_local_build,
            required_features: self.required_system_features(),
        }
    }
}

/// Resolve a package name. Prefers a non-empty `env_pname`; otherwise strips a
/// trailing `-<version>` (version starts with a digit) from the derivation name.
pub fn derive_pname(env_pname: Option<&str>, name: &str) -> Option<String> {
    if let Some(p) = env_pname
        && !p.is_empty()
    {
        return Some(p.to_owned());
    }
    if name.is_empty() {
        return None;
    }
    match name.rsplit_once('-') {
        Some((prefix, version))
            if version.chars().next().is_some_and(|c| c.is_ascii_digit()) =>
        {
            Some(prefix.to_owned())
        }
        _ => Some(name.to_owned()),
    }
}

// ── Low-level parsers ─────────────────────────────────────────────────────────

/// Parses a double-quoted ATerm string. Returns `(value, remaining_input)`.
fn parse_string(s: &str) -> Result<(String, &str)> {
    let s = s.trim_start();
    let s = s
        .strip_prefix('"')
        .ok_or_else(|| anyhow!("expected '\"'"))?;
    let mut result = String::new();
    let mut iter = s.char_indices();
    loop {
        match iter.next() {
            Some((i, '"')) => return Ok((result, &s[i + 1..])),
            Some((_, '\\')) => match iter.next() {
                Some((_, 'n')) => result.push('\n'),
                Some((_, 't')) => result.push('\t'),
                Some((_, 'r')) => result.push('\r'),
                Some((_, c)) => result.push(c),
                None => return Err(anyhow!("unexpected end of input inside escape")),
            },
            Some((_, c)) => result.push(c),
            None => return Err(anyhow!("unterminated string literal")),
        }
    }
}

/// Advances past optional leading whitespace and one comma. Returns the rest.
fn comma(s: &str) -> Result<&str> {
    let s = s.trim_start();
    s.strip_prefix(',')
        .ok_or_else(|| anyhow!("expected ','"))
        .map(|r| r.trim_start())
}

// ── Field parsers ─────────────────────────────────────────────────────────────

/// Parses `[("name","path","algo","hash"),...]`.
fn parse_outputs(s: &str) -> Result<(Vec<DerivationOutput>, &str)> {
    let s = s.trim_start();
    let mut s = s
        .strip_prefix('[')
        .ok_or_else(|| anyhow!("expected '[' for outputs"))?;
    let mut outputs = Vec::new();
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix(']') {
            return Ok((outputs, r));
        }
        if let Some(r) = s.strip_prefix(',') {
            s = r.trim_start();
        }
        if let Some(r) = s.strip_prefix(']') {
            return Ok((outputs, r));
        }

        s = s
            .strip_prefix('(')
            .ok_or_else(|| anyhow!("expected '(' for output entry"))?;
        let (name, r) = parse_string(s)?;
        s = comma(r)?;
        let (path, r) = parse_string(s)?;
        s = comma(r)?;
        let (hash_algo, r) = parse_string(s)?;
        s = comma(r)?;
        let (hash, r) = parse_string(s)?;
        s = r.trim_start();
        s = s
            .strip_prefix(')')
            .ok_or_else(|| anyhow!("expected ')' to close output entry"))?;
        outputs.push(DerivationOutput {
            name,
            path,
            hash_algo,
            hash,
        });
    }
}

type InputDrv = (String, Vec<String>);

/// Parses `[("/nix/store/hash.drv",["out","dev"]),...]`.
fn parse_input_drvs(s: &str) -> Result<(Vec<InputDrv>, &str)> {
    let s = s.trim_start();
    let mut s = s
        .strip_prefix('[')
        .ok_or_else(|| anyhow!("expected '[' for inputDrvs"))?;
    let mut drvs = Vec::new();
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix(']') {
            return Ok((drvs, r));
        }
        if let Some(r) = s.strip_prefix(',') {
            s = r.trim_start();
        }
        if let Some(r) = s.strip_prefix(']') {
            return Ok((drvs, r));
        }

        s = s
            .strip_prefix('(')
            .ok_or_else(|| anyhow!("expected '(' for inputDrv entry"))?;
        let (path, r) = parse_string(s)?;
        s = comma(r)?;
        let (outputs, r) = parse_string_list(s)?;
        s = r.trim_start();
        s = s
            .strip_prefix(')')
            .ok_or_else(|| anyhow!("expected ')' to close inputDrv entry"))?;
        drvs.push((path, outputs));
    }
}

/// Parses `["str1","str2",...]` into a `Vec<String>`.
fn parse_string_list(s: &str) -> Result<(Vec<String>, &str)> {
    let s = s.trim_start();
    let mut s = s
        .strip_prefix('[')
        .ok_or_else(|| anyhow!("expected '[' for string list"))?;
    let mut items = Vec::new();
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix(']') {
            return Ok((items, r));
        }
        if let Some(r) = s.strip_prefix(',') {
            s = r.trim_start();
        }
        if let Some(r) = s.strip_prefix(']') {
            return Ok((items, r));
        }
        let (item, r) = parse_string(s)?;
        s = r;
        items.push(item);
    }
}

/// Parses the environment list `[("KEY","VALUE"),...]` into a `HashMap`.
fn parse_env(s: &str) -> Result<(HashMap<String, String>, &str)> {
    let s = s.trim_start();
    let mut s = s
        .strip_prefix('[')
        .ok_or_else(|| anyhow!("expected '[' for env list"))?;
    let mut map = HashMap::new();
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix(']') {
            return Ok((map, r));
        }
        if let Some(r) = s.strip_prefix(',') {
            s = r.trim_start();
        }
        if let Some(r) = s.strip_prefix(']') {
            return Ok((map, r));
        }

        s = s
            .strip_prefix('(')
            .ok_or_else(|| anyhow!("expected '(' for env entry"))?;
        let (key, r) = parse_string(s)?;
        s = comma(r)?;
        let (value, r) = parse_string(s)?;
        s = r.trim_start();
        s = s
            .strip_prefix(')')
            .ok_or_else(|| anyhow!("expected ')' to close env entry"))?;
        map.insert(key, value);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Parses the raw bytes of a `.drv` file into a [`Derivation`].
pub fn parse_drv(content: &[u8]) -> Result<Derivation> {
    let content = std::str::from_utf8(content)
        .map_err(|e| anyhow!("drv is not valid UTF-8: {}", e))?
        .trim();

    let mut s = content
        .strip_prefix("Derive(")
        .ok_or_else(|| anyhow!("not a derivation file: does not start with 'Derive('"))?;

    let (outputs, r) = parse_outputs(s)?;
    s = comma(r)?;
    let (input_derivations, r) = parse_input_drvs(s)?;
    s = comma(r)?;
    let (input_sources, r) = parse_string_list(s)?;
    s = comma(r)?;
    let (system, r) = parse_string(s)?;
    s = comma(r)?;
    let (builder, r) = parse_string(s)?;
    s = comma(r)?;
    let (args, r) = parse_string_list(s)?;
    s = comma(r)?;
    let (environment, _) = parse_env(s)?;

    Ok(Derivation {
        outputs,
        input_derivations,
        input_sources,
        system,
        builder,
        args,
        environment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &[u8] = br#"Derive([("out","/nix/store/abc-hello","","")],[("/nix/store/xyz.drv",["out"])],["/nix/store/src"],"x86_64-linux","/nix/store/bash",["-e","/nix/store/builder.sh"],[("name","hello"),("requiredSystemFeatures","kvm big-parallel"),("system","x86_64-linux")])"#;

    #[test]
    fn test_parse_full() {
        let drv = parse_drv(EXAMPLE).unwrap();
        assert_eq!(drv.system, "x86_64-linux");
        assert_eq!(drv.builder, "/nix/store/bash");
        assert_eq!(drv.args, vec!["-e", "/nix/store/builder.sh"]);
        assert_eq!(drv.outputs.len(), 1);
        assert_eq!(drv.outputs[0].name, "out");
        assert_eq!(drv.outputs[0].path, "/nix/store/abc-hello");
        assert_eq!(drv.input_derivations.len(), 1);
        assert_eq!(drv.input_derivations[0].0, "/nix/store/xyz.drv");
        assert_eq!(drv.input_derivations[0].1, vec!["out"]);
        assert_eq!(drv.input_sources, vec!["/nix/store/src"]);
        assert_eq!(drv.environment["name"], "hello");
    }

    #[test]
    fn test_required_system_features() {
        let drv = parse_drv(EXAMPLE).unwrap();
        assert_eq!(drv.required_system_features(), vec!["kvm", "big-parallel"]);
    }

    #[test]
    fn test_no_features() {
        let drv = br#"Derive([("out","/nix/store/abc-hello","","")],[],["/nix/store/src"],"aarch64-linux","/nix/store/bash",[],[("name","hello")])"#;
        let drv = parse_drv(drv).unwrap();
        assert_eq!(drv.system, "aarch64-linux");
        assert!(drv.required_system_features().is_empty());
    }

    const META_DRV: &[u8] = br#"Derive([("out","/nix/store/abc-hello","","")],[],["/nix/store/src"],"x86_64-linux","/nix/store/bash",[],[("name","hello"),("requiredSystemFeatures","kvm"),("timeout","3600"),("maxSilent","1800"),("preferLocalBuild","1")])"#;

    #[test]
    fn build_meta_reads_all_fields() {
        let drv = parse_drv(META_DRV).unwrap();
        let meta = drv.build_meta();
        assert_eq!(meta.timeout_secs, Some(3600));
        assert_eq!(meta.max_silent_secs, Some(1800));
        assert!(meta.prefer_local_build);
        assert_eq!(meta.required_features, vec!["kvm"]);
    }

    #[test]
    fn build_meta_defaults_when_absent() {
        let drv = parse_drv(EXAMPLE).unwrap();
        let meta = drv.build_meta();
        assert_eq!(meta.timeout_secs, None);
        assert_eq!(meta.max_silent_secs, None);
        assert!(!meta.prefer_local_build);
        assert_eq!(meta.required_features, vec!["kvm", "big-parallel"]);
    }

    #[test]
    fn build_meta_prefer_local_build_accepts_true_and_1() {
        let true_drv = br#"Derive([("out","/nix/store/abc-hello","","")],[],[],"x86_64-linux","/nix/store/bash",[],[("name","x"),("preferLocalBuild","true")])"#;
        assert!(parse_drv(true_drv).unwrap().build_meta().prefer_local_build);
    }

    #[test]
    fn build_meta_ignores_unparseable_timeout() {
        let bad = br#"Derive([("out","/nix/store/abc-hello","","")],[],[],"x86_64-linux","/nix/store/bash",[],[("name","x"),("timeout","forever")])"#;
        assert_eq!(parse_drv(bad).unwrap().build_meta().timeout_secs, None);
    }

    #[test]
    fn pname_prefers_env_then_strips_version() {
        assert_eq!(derive_pname(Some("hello"), "hello-2.12.1"), Some("hello".into()));
        assert_eq!(derive_pname(None, "hello-2.12.1"), Some("hello".into()));
        assert_eq!(derive_pname(None, "hello"), Some("hello".into()));
        assert_eq!(derive_pname(None, "gcc-wrapper-13.2.0"), Some("gcc-wrapper".into()));
        assert_eq!(derive_pname(Some(""), "hello-1.0"), Some("hello".into()));
    }

    #[test]
    fn allow_substitutes_defaults_true_and_parses_false() {
        let absent = br#"Derive([("out","/nix/store/a","","")],[],[],"x86_64-linux","/nix/store/bash",[],[("name","x")])"#;
        assert!(parse_drv(absent).unwrap().allow_substitutes());
        let empty = br#"Derive([("out","/nix/store/a","","")],[],[],"x86_64-linux","/nix/store/bash",[],[("name","x"),("allowSubstitutes","")])"#;
        assert!(!parse_drv(empty).unwrap().allow_substitutes());
        let zero = br#"Derive([("out","/nix/store/a","","")],[],[],"x86_64-linux","/nix/store/bash",[],[("name","x"),("allowSubstitutes","0")])"#;
        assert!(!parse_drv(zero).unwrap().allow_substitutes());
        let on = br#"Derive([("out","/nix/store/a","","")],[],[],"x86_64-linux","/nix/store/bash",[],[("name","x"),("allowSubstitutes","1")])"#;
        assert!(parse_drv(on).unwrap().allow_substitutes());
    }
}
