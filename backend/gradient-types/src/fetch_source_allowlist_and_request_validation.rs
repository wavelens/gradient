/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, HashMap};

use gradient_wire::content_addressed_store_path_from_name_address_references::path_for;
use harmonia_store_content_address::ContentAddress;
use harmonia_store_path::{StoreDir, StorePathSet};
use harmonia_utils_hash::fmt::Any;
use harmonia_utils_hash::{Hash, Sha256};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::flake_url::{check_authority, split_host_port};
use crate::input::InputError;

pub const ALLOWED_URI_SCHEMES_FOR_ALL_REMOTE_SOURCES: &[&str] = &[
    "github:",
    "gitlab:",
    "sourcehut:",
    "git+https:",
    "git+http:",
    "git+ssh:",
    "git+git:",
    "hg+https:",
    "hg+ssh:",
    "https:",
    "http:",
    "tarball+https:",
    "tarball+http:",
    "file+https:",
    "file+http:",
];

const FETCH_TREE_INPUT_TYPES: &[&str] = &[
    "github",
    "gitlab",
    "sourcehut",
    "git",
    "hg",
    "tarball",
    "file",
];
const GIT_URL_SCHEMES: &[&str] = &["https", "http", "ssh", "git"];
const MERCURIAL_URL_SCHEMES: &[&str] = &["https", "ssh"];
const DOWNLOAD_URL_SCHEMES: &[&str] = &["https", "http"];
const OVERRIDE_URL_SCHEMES: &[&str] = &[
    "git+https",
    "git+http",
    "git+ssh",
    "git+git",
    "git",
    "hg+https",
    "hg+ssh",
    "https",
    "http",
    "tarball+https",
    "tarball+http",
    "file+https",
    "file+http",
];
const OVERRIDE_HOSTED_PREFIXES: &[&str] = &["github:", "gitlab:", "sourcehut:"];
const NIX_STORE_DIR: &str = "/nix/store";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchRequestBuiltin {
    #[serde(rename = "fetchTree")]
    FetchTree,
    #[serde(rename = "fetchTarball")]
    FetchTarball,
    #[serde(rename = "fetchurl")]
    Fetchurl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedFetchTreeInputAttributes(BTreeMap<String, Value>);

impl ValidatedFetchTreeInputAttributes {
    pub fn check(attributes: &Map<String, Value>) -> Result<Self, InputError> {
        let input_type = attributes
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| InputError::InvalidFetchRequest("an input without a type".into()))?;
        if !FETCH_TREE_INPUT_TYPES.contains(&input_type) {
            return Err(InputError::InputTypeNotAllowed(input_type.to_owned()));
        }

        let mut checked = BTreeMap::new();
        for (name, value) in attributes {
            let scalar = match value {
                Value::String(_) | Value::Bool(_) => true,
                Value::Number(number) => number.is_u64() || number.is_i64(),
                _ => false,
            };
            if !scalar {
                return Err(InputError::AttributeValueIsNotStringIntegerOrBoolean(
                    name.clone(),
                ));
            }
            match (name.as_str(), value.as_str()) {
                ("path", _) => return Err(InputError::InputPathNamesLocalFile),
                ("url", Some(url)) => {
                    check_url_attribute_schemes_and_require_host_for_input_type(input_type, url)?
                }
                ("url", None) => return Err(InputError::NotAllowedUrl(value.to_string())),
                ("host", Some(host)) if is_valid_host_name(host) => {}
                ("host", _) => return Err(InputError::InvalidHost(value.to_string())),
                _ => {}
            }
            checked.insert(name.clone(), value.clone());
        }

        if matches!(input_type, "git" | "hg" | "tarball" | "file") && !checked.contains_key("url") {
            return Err(InputError::NotAllowedUrl(String::new()));
        }
        Ok(Self(checked))
    }

    pub fn url_to_fetch(&self) -> Option<&str> {
        self.0.get("url").and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchUrlOrFetchTarballRequestArguments {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchRequestArguments {
    Tree(ValidatedFetchTreeInputAttributes),
    Url(FetchUrlOrFetchTarballRequestArguments),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedFetchRequest {
    builtin: FetchRequestBuiltin,
    arguments: FetchRequestArguments,
    store_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnvalidatedFetchRequest {
    builtin: FetchRequestBuiltin,
    args: Map<String, Value>,
    #[serde(rename = "storePath", default)]
    store_path: Option<String>,
}

#[derive(Serialize)]
struct AcceptedFetchRequestCanonicalJson<'a> {
    builtin: FetchRequestBuiltin,
    args: Value,
    #[serde(rename = "storePath", skip_serializing_if = "Option::is_none")]
    store_path: Option<&'a str>,
}

impl AllowedFetchRequest {
    pub fn parse(request: &str) -> Result<Self, InputError> {
        let received: UnvalidatedFetchRequest = serde_json::from_str(request)
            .map_err(|e| InputError::InvalidFetchRequest(e.to_string()))?;
        let arguments = match received.builtin {
            FetchRequestBuiltin::FetchTree => FetchRequestArguments::Tree(
                ValidatedFetchTreeInputAttributes::check(&received.args)?,
            ),
            FetchRequestBuiltin::FetchTarball | FetchRequestBuiltin::Fetchurl => {
                let arguments: FetchUrlOrFetchTarballRequestArguments =
                    serde_json::from_value(Value::Object(received.args))
                        .map_err(|e| InputError::InvalidFetchRequest(e.to_string()))?;
                check_url_attribute_schemes_and_require_host_for_input_type(
                    "file",
                    &arguments.url,
                )?;
                FetchRequestArguments::Url(arguments)
            }
        };
        Self::new(received.builtin, arguments, received.store_path)
    }

    pub fn build_accepted_fetch_tree_request_from_validated_attributes_and_expected_store_path(
        attributes: ValidatedFetchTreeInputAttributes,
        store_path: Option<String>,
    ) -> Result<Self, InputError> {
        Self::new(
            FetchRequestBuiltin::FetchTree,
            FetchRequestArguments::Tree(attributes),
            store_path,
        )
    }

    fn new(
        builtin: FetchRequestBuiltin,
        arguments: FetchRequestArguments,
        store_path: Option<String>,
    ) -> Result<Self, InputError> {
        if let Some(path) = &store_path {
            let store_dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
            if !is_exactly_one_store_path_under_store_dir(store_dir, path) {
                return Err(InputError::NotExactlyOneStorePath(path.clone()));
            }
        }
        Ok(Self {
            builtin,
            arguments,
            store_path,
        })
    }

    pub fn store_path(&self) -> Option<&str> {
        self.store_path.as_deref()
    }

    pub fn pin(&self) -> Option<String> {
        let (hash, name, content_address): (_, _, fn(Hash) -> ContentAddress) =
            match (self.builtin, &self.arguments) {
                (_, FetchRequestArguments::Tree(attributes)) => (
                    attributes.0.get("narHash")?.as_str()?,
                    attributes
                        .0
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("source"),
                    ContentAddress::NixArchive,
                ),
                (FetchRequestBuiltin::FetchTarball, FetchRequestArguments::Url(arguments)) => (
                    arguments.sha256.as_deref()?,
                    arguments.name.as_deref().unwrap_or("source"),
                    ContentAddress::NixArchive,
                ),
                (_, FetchRequestArguments::Url(arguments)) => (
                    arguments.sha256.as_deref()?,
                    arguments.name.as_deref().unwrap_or_else(|| {
                        let url = arguments.url.trim_end_matches('/');
                        url.rsplit_once('/').map_or(url, |(_, last)| last)
                    }),
                    ContentAddress::Flat,
                ),
            };
        let digest = hash.parse::<Any<Sha256>>().ok()?.into_hash();
        let path = path_for(
            name,
            &content_address(Hash::SHA256(digest)),
            &StorePathSet::new(),
        )
        .ok()?;
        Some(StoreDir::default().display(&path).to_string())
    }

    pub fn url_to_fetch(&self) -> Option<&str> {
        match &self.arguments {
            FetchRequestArguments::Tree(attributes) => attributes.url_to_fetch(),
            FetchRequestArguments::Url(arguments) => Some(&arguments.url),
        }
    }

    pub fn get_canonical_fetch_request_json(&self) -> String {
        let args = match &self.arguments {
            FetchRequestArguments::Tree(attributes) => {
                Value::Object(attributes.0.clone().into_iter().collect())
            }
            FetchRequestArguments::Url(arguments) => {
                serde_json::to_value(arguments).expect("plain strings serialise")
            }
        };
        serde_json::to_string(&AcceptedFetchRequestCanonicalJson {
            builtin: self.builtin,
            args,
            store_path: self.store_path.as_deref(),
        })
        .expect("plain JSON values serialise")
    }
}

fn check_url_attribute_schemes_and_require_host_for_input_type(
    input_type: &str,
    url: &str,
) -> Result<(), InputError> {
    let refused = || InputError::NotAllowedUrl(url.to_owned());
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(refused());
    }
    let (allowed, scheme_prefix) = match input_type {
        "git" => (GIT_URL_SCHEMES, "git+"),
        "hg" => (MERCURIAL_URL_SCHEMES, "hg+"),
        _ => (DOWNLOAD_URL_SCHEMES, ""),
    };
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.strip_prefix(scheme_prefix).unwrap_or(scheme);
            if allowed.contains(&scheme) && has_valid_authority(scheme, rest) {
                Ok(())
            } else {
                Err(refused())
            }
        }
        None if input_type == "git" && is_scp_like_git_url(url) => Ok(()),
        None => Err(refused()),
    }
}

fn has_valid_authority(scheme: &str, rest: &str) -> bool {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    check_authority(scheme, authority).is_ok()
        && split_host_port(host_port).is_some_and(|(host, _)| is_valid_host_name(host))
}

fn is_valid_host_name(host: &str) -> bool {
    !host.is_empty()
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
}

fn is_scp_like_git_url(url: &str) -> bool {
    let Some((user_host, path)) = url.split_once(':') else {
        return false;
    };
    let Some((user, host)) = user_host.rsplit_once('@') else {
        return false;
    };
    !user.is_empty()
        && !user.starts_with('-')
        && !user_host.contains('/')
        && is_valid_host_name(host)
        && !path.is_empty()
}

fn is_exactly_one_store_path_under_store_dir(store_dir: &str, path: &str) -> bool {
    let Some(base) = path
        .strip_prefix(store_dir)
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return false;
    };
    let store_dir_ok = store_dir.starts_with('/')
        && store_dir[1..]
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."));
    store_dir_ok && crate::OriginalStorePath::parse_store_path_base_name(base).is_ok()
}

pub fn check_flake_input_override_reference_is_remote_flake_or_single_store_path(
    input_name: &str,
    flake_ref: &str,
) -> Result<(), InputError> {
    let accepted = !flake_ref
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
        && if let Some(path) = flake_ref.strip_prefix("path:") {
            !path.contains('?') && is_exactly_one_store_path_under_store_dir(NIX_STORE_DIR, path)
        } else if let Some(id) = flake_ref.strip_prefix("flake:") {
            id.starts_with(|c: char| c.is_ascii_alphanumeric())
        } else if let Some(rest) = OVERRIDE_HOSTED_PREFIXES
            .iter()
            .find_map(|prefix| flake_ref.strip_prefix(prefix))
        {
            rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '~')
        } else {
            flake_ref.split_once("://").is_some_and(|(scheme, rest)| {
                OVERRIDE_URL_SCHEMES.contains(&scheme) && has_valid_authority(scheme, rest)
            })
        };

    if accepted {
        Ok(())
    } else {
        Err(InputError::FlakeRefNotRemote {
            input_name: input_name.to_owned(),
            flake_ref: flake_ref.to_owned(),
        })
    }
}

pub fn check_allowed_uri_prefix_has_remote_nix_scheme(prefix: &str) -> Result<String, InputError> {
    if prefix.is_empty() || prefix.contains(char::is_whitespace) {
        return Err(InputError::ExtraAllowedUrlPrefixEmptyOrWhitespace(
            prefix.to_owned(),
        ));
    }
    if !ALLOWED_URI_SCHEMES_FOR_ALL_REMOTE_SOURCES
        .iter()
        .any(|scheme| prefix.starts_with(scheme))
    {
        return Err(InputError::AllowedUriPrefixNotRemoteNixScheme(
            prefix.to_owned(),
        ));
    }
    Ok(prefix.to_owned())
}

#[derive(Debug, Clone)]
pub struct ValidatedFlakeLock {
    bytes: Vec<u8>,
    json: Value,
    inputs: HashMap<String, ValidatedFetchTreeInputAttributes>,
}

#[derive(Debug, Clone)]
struct FlakeLockNodeSourceDirectory {
    tree: Option<String>,
    components: Vec<String>,
}

impl ValidatedFlakeLock {
    pub fn parse(bytes: Vec<u8>, flake_dir: Option<&str>) -> Result<Self, InputError> {
        let json: Value = serde_json::from_slice(&bytes)
            .map_err(|e| InputError::FlakeLockNotValidJsonOrMissingNodes(e.to_string()))?;
        let inputs = check_lock_nodes(&json, flake_dir)?;
        Ok(Self {
            bytes,
            json,
            inputs,
        })
    }

    pub fn parsed_json_value(&self) -> &Value {
        &self.json
    }

    pub fn raw_lock_file_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn validated_fetch_tree_input_by_node_key(
        &self,
        key: &str,
    ) -> Option<&ValidatedFetchTreeInputAttributes> {
        self.inputs.get(key)
    }
}

pub fn validate_flake_lock_computed_during_evaluation(
    lock_file: &Value,
    flake_dir: Option<&str>,
    override_references: &[&str],
) -> Result<(), InputError> {
    let mut lock_file = lock_file.clone();
    let override_store_paths: Vec<&str> = override_references
        .iter()
        .filter_map(|reference| reference.strip_prefix("path:"))
        .map(|path| path.split(['?', '#']).next().unwrap_or(path))
        .filter(|path| is_exactly_one_store_path_under_store_dir(NIX_STORE_DIR, path))
        .collect();
    for node in lock_file
        .get_mut("nodes")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|nodes| nodes.values_mut())
    {
        let locked = node.get("locked");
        let names_override_store_path = locked
            .and_then(|locked| locked.get("type"))
            .and_then(Value::as_str)
            == Some("path")
            && locked
                .and_then(|locked| locked.get("path"))
                .and_then(Value::as_str)
                .is_some_and(|path| override_store_paths.contains(&path));
        if names_override_store_path && let Some(node) = node.as_object_mut() {
            node.remove("locked");
        }
    }
    check_lock_nodes(&lock_file, flake_dir).map(drop)
}

fn check_lock_nodes(
    json: &Value,
    flake_dir: Option<&str>,
) -> Result<HashMap<String, ValidatedFetchTreeInputAttributes>, InputError> {
    let malformed = |detail: &str| InputError::FlakeLockNotValidJsonOrMissingNodes(detail.into());
    let nodes = json
        .get("nodes")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("no nodes"))?;
    let root = json.get("root").and_then(Value::as_str).unwrap_or("root");
    if !nodes.contains_key(root) {
        return Err(malformed("no root node"));
    }

    let mut directories: HashMap<&str, FlakeLockNodeSourceDirectory> = HashMap::new();
    directories.insert(
        root,
        FlakeLockNodeSourceDirectory {
            tree: None,
            components: flake_dir
                .map(|dir| dir.split('/').map(str::to_owned).collect())
                .unwrap_or_default(),
        },
    );
    let mut inputs = HashMap::new();
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        let children = nodes[parent]
            .get("inputs")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(_, edge)| edge.as_str());
        for child in children {
            if directories.contains_key(child) {
                continue;
            }
            let node = nodes
                .get(child)
                .ok_or_else(|| malformed("an input names a missing node"))?;
            let refused = |reason| InputError::FlakeLockNodeValidationFailed {
                input: child.to_owned(),
                reason: Box::new(reason),
            };
            let directory = match node.get("locked").and_then(Value::as_object) {
                None => FlakeLockNodeSourceDirectory {
                    tree: Some(child.to_owned()),
                    components: Vec::new(),
                },
                Some(locked) if locked.get("type").and_then(Value::as_str) == Some("path") => {
                    let path = locked
                        .get("path")
                        .and_then(Value::as_str)
                        .filter(|path| !path.starts_with('/'))
                        .ok_or_else(|| refused(InputError::InputPathNamesLocalFile))?;
                    let base = match node.get("parent").and_then(Value::as_array) {
                        Some(names) => {
                            let declarer = node_by_input_names(nodes, root, names)
                                .ok_or_else(|| malformed("a parent names a missing input"))?;
                            directories
                                .get(declarer)
                                .ok_or_else(|| malformed("a parent is not reachable"))?
                        }
                        None => &directories[parent],
                    };
                    join_inside_tree(base, path).ok_or_else(|| {
                        refused(InputError::RelativePathLeavesSourceTree(path.to_owned()))
                    })?
                }
                Some(locked) => {
                    let mut attributes = locked.clone();
                    let dir = attributes.remove("dir");
                    inputs.insert(
                        child.to_owned(),
                        ValidatedFetchTreeInputAttributes::check(&attributes).map_err(refused)?,
                    );
                    FlakeLockNodeSourceDirectory {
                        tree: Some(child.to_owned()),
                        components: dir
                            .as_ref()
                            .and_then(Value::as_str)
                            .map(|dir| dir.split('/').map(str::to_owned).collect())
                            .unwrap_or_default(),
                    }
                }
            };
            directories.insert(child, directory);
            queue.push(child);
        }
    }
    Ok(inputs)
}

fn node_by_input_names<'a>(
    nodes: &'a Map<String, Value>,
    root: &'a str,
    names: &[Value],
) -> Option<&'a str> {
    names.iter().try_fold(root, |key, name| {
        nodes.get(key)?.get("inputs")?.get(name.as_str()?)?.as_str()
    })
}

fn join_inside_tree(
    base: &FlakeLockNodeSourceDirectory,
    path: &str,
) -> Option<FlakeLockNodeSourceDirectory> {
    let mut components = base.components.clone();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            name => components.push(name.to_owned()),
        }
    }
    Some(FlakeLockNodeSourceDirectory {
        tree: base.tree.clone(),
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAR_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const STORE_PATH: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source";

    fn tree(args: Value) -> String {
        serde_json::json!({ "builtin": "fetchTree", "args": args }).to_string()
    }

    fn refused(request: &str) -> InputError {
        AllowedFetchRequest::parse(request).expect_err(request)
    }

    #[test]
    fn a_path_input_naming_a_host_file_is_refused() {
        let request = tree(serde_json::json!({
            "type": "path", "path": "/run/credentials/x", "narHash": NAR_HASH
        }));
        assert_eq!(
            refused(&request),
            InputError::InputTypeNotAllowed("path".into())
        );
    }

    #[test]
    fn fetchurl_and_fetch_tarball_of_a_local_file_are_refused() {
        for builtin in ["fetchurl", "fetchTarball"] {
            let request = serde_json::json!({
                "builtin": builtin, "args": { "url": "file:///etc/passwd", "name": "source" }
            })
            .to_string();
            assert_eq!(
                refused(&request),
                InputError::NotAllowedUrl("file:///etc/passwd".into())
            );
        }
    }

    #[test]
    fn tarball_and_file_inputs_with_a_file_url_are_refused() {
        for input_type in ["tarball", "file"] {
            for url in [
                "file:///etc/passwd",
                "FILE:///etc/passwd",
                "ftp://h/x.tar.gz",
                "s3://b/x.tar.gz",
                "channel:nixos-unstable",
            ] {
                let request = tree(serde_json::json!({ "type": input_type, "url": url }));
                assert!(
                    matches!(refused(&request), InputError::NotAllowedUrl(_)),
                    "{input_type} {url}"
                );
            }
        }
    }

    #[test]
    fn a_git_input_with_a_local_url_is_refused() {
        for url in [
            "/srv/repo",
            "file:///srv/repo",
            "./repo",
            "git+file:///srv/repo",
            "Git+File:///srv/repo",
            "ssh://-oProxyCommand=x/repo",
            "https:///repo",
        ] {
            let request = tree(serde_json::json!({ "type": "git", "url": url }));
            assert!(
                matches!(refused(&request), InputError::NotAllowedUrl(_)),
                "{url}"
            );
        }
    }

    #[test]
    fn a_mercurial_input_with_a_local_url_is_refused() {
        for url in [
            "/srv/repo",
            "file:///srv/repo",
            "hg+file:///srv/repo",
            "http://h/repo",
            "Hg+File:///srv/repo",
        ] {
            let request = tree(serde_json::json!({ "type": "hg", "url": url }));
            assert!(
                matches!(refused(&request), InputError::NotAllowedUrl(_)),
                "{url}"
            );
        }
    }

    #[test]
    fn a_git_input_with_a_path_attribute_is_refused() {
        let request = tree(serde_json::json!({
            "type": "git", "url": "https://example.com/r.git", "path": "/etc"
        }));
        assert_eq!(refused(&request), InputError::InputPathNamesLocalFile);
    }

    #[test]
    fn unknown_types_and_nested_attributes_are_refused() {
        for input_type in ["indirect", "mercurial", "sqlite", "PATH", "Path", "Git"] {
            let request = tree(serde_json::json!({ "type": input_type, "url": "https://h/x" }));
            assert_eq!(
                refused(&request),
                InputError::InputTypeNotAllowed(input_type.into())
            );
        }
        let nested =
            tree(serde_json::json!({ "type": "github", "owner": "o", "repo": { "a": 1 } }));
        assert_eq!(
            refused(&nested),
            InputError::AttributeValueIsNotStringIntegerOrBoolean("repo".into())
        );
    }

    #[test]
    fn a_store_path_with_extra_segments_is_refused() {
        let request = serde_json::json!({
            "builtin": "fetchTree",
            "args": { "type": "github", "owner": "o", "repo": "r" },
            "storePath": format!("{STORE_PATH}/../../../run/x"),
        })
        .to_string();
        assert!(matches!(
            refused(&request),
            InputError::NotExactlyOneStorePath(_)
        ));
    }

    #[test]
    fn remote_inputs_are_accepted_and_keep_their_canonical_form() {
        for request in [
            format!(
                r#"{{"builtin":"fetchTree","args":{{"narHash":"{NAR_HASH}","owner":"NixOS","repo":"nixpkgs","rev":"r","type":"github"}},"storePath":"{STORE_PATH}"}}"#
            ),
            r#"{"builtin":"fetchTree","args":{"ref":"main","type":"git","url":"https://example.com/r.git"}}"#.to_owned(),
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"git+ssh://git@example.com/r"}}"#.to_owned(),
            r#"{"builtin":"fetchTree","args":{"type":"git","url":"git@example.com:o/r.git"}}"#.to_owned(),
            r#"{"builtin":"fetchTree","args":{"type":"hg","url":"https://example.com/r"}}"#.to_owned(),
            r#"{"builtin":"fetchTree","args":{"type":"hg","url":"hg+ssh://hg@example.com/r"}}"#.to_owned(),
            r#"{"builtin":"fetchTree","args":{"lastModified":7,"shallow":true,"type":"tarball","url":"https://example.com/x.tar.gz"}}"#.to_owned(),
            r#"{"builtin":"fetchurl","args":{"name":"x","sha256":"sha256-x","url":"https://example.com/x"}}"#.to_owned(),
        ] {
            let parsed = AllowedFetchRequest::parse(&request).expect(&request);
            assert_eq!(parsed.get_canonical_fetch_request_json(), request);
        }
    }

    fn lock(nodes: Value) -> Vec<u8> {
        serde_json::json!({ "root": "root", "version": 7, "nodes": nodes })
            .to_string()
            .into_bytes()
    }

    #[test]
    fn a_lock_with_an_absolute_path_node_is_refused() {
        let bytes = lock(serde_json::json!({
            "root": { "inputs": { "secret": "secret" } },
            "secret": { "locked": { "type": "path", "path": "/run/credentials/x", "narHash": NAR_HASH } },
        }));
        assert_eq!(
            ValidatedFlakeLock::parse(bytes, None).unwrap_err(),
            InputError::FlakeLockNodeValidationFailed {
                input: "secret".into(),
                reason: Box::new(InputError::InputPathNamesLocalFile),
            }
        );
    }

    #[test]
    fn a_lock_with_a_local_git_node_is_refused() {
        let bytes = lock(serde_json::json!({
            "root": { "inputs": { "a": "a" } },
            "a": { "inputs": { "b": "b" }, "locked": { "type": "github", "owner": "o", "repo": "a" } },
            "b": { "locked": { "type": "git", "url": "file:///srv/repo" } },
        }));
        assert!(matches!(
            ValidatedFlakeLock::parse(bytes, None).unwrap_err(),
            InputError::FlakeLockNodeValidationFailed { input, .. } if input == "b"
        ));
    }

    #[test]
    fn relative_path_nodes_inside_the_tree_are_accepted_and_never_fetched() {
        let bytes = lock(serde_json::json!({
            "root": { "inputs": { "sub": "sub", "nixpkgs": "nixpkgs" } },
            "sub": { "inputs": { "lib": "lib" }, "locked": { "type": "path", "path": "./sub" }, "parent": [] },
            "lib": { "locked": { "type": "path", "path": "../lib" }, "parent": ["sub"] },
            "nixpkgs": { "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "dir": "lib" } },
        }));
        let lock = ValidatedFlakeLock::parse(bytes, None).unwrap();
        assert!(lock.validated_fetch_tree_input_by_node_key("sub").is_none());
        assert!(lock.validated_fetch_tree_input_by_node_key("lib").is_none());
        assert_eq!(
            lock.validated_fetch_tree_input_by_node_key("nixpkgs")
                .map(|input| input.0.contains_key("dir")),
            Some(false)
        );
    }

    #[test]
    fn a_relative_path_node_leaving_the_tree_is_refused() {
        let nodes = serde_json::json!({
            "root": { "inputs": { "up": "up" } },
            "up": { "locked": { "type": "path", "path": "../../.." }, "parent": [] },
        });
        assert_eq!(
            ValidatedFlakeLock::parse(lock(nodes.clone()), Some("a/b")).unwrap_err(),
            InputError::FlakeLockNodeValidationFailed {
                input: "up".into(),
                reason: Box::new(InputError::RelativePathLeavesSourceTree("../../..".into())),
            }
        );
        assert!(ValidatedFlakeLock::parse(lock(nodes), Some("a/b/c")).is_ok());
    }

    #[test]
    fn overrides_accept_remote_refs_and_one_store_path() {
        for flake_ref in [
            "github:NixOS/nixpkgs",
            "gitlab:group/task",
            "sourcehut:~user/task",
            "git+ssh://git@h/x.git",
            "git+https://h/x.git",
            "git+http://h/x.git",
            "git://h/x.git",
            "https://h/x.tar.gz",
            "http://h/x.tar.gz",
            "flake:nixpkgs",
            "hg+https://h/x",
            "hg+ssh://hg@h/x",
            "path:/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source",
        ] {
            assert!(
                check_flake_input_override_reference_is_remote_flake_or_single_store_path(
                    "nixpkgs", flake_ref
                )
                .is_ok(),
                "{flake_ref}"
            );
        }
    }

    #[test]
    fn overrides_refuse_local_paths_and_store_path_traversal() {
        for flake_ref in [
            "path:/nix/store/../../run/x",
            "path:/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-source/../../run/x",
            "path:/nix/store/",
            "path:/home/u/x",
            "git+file:///srv/repo",
            "file:///etc/passwd",
            "https:///x",
            "/abs",
            "./rel",
            "name",
            "github:a/b c",
            "PATH:/etc/passwd",
            "Git+File:///srv/repo",
            "FILE:///etc/passwd",
            format!("path:{STORE_PATH}?dir=../../run/x").as_str(),
            format!("path:{STORE_PATH}?url=file:///etc/passwd").as_str(),
            format!("path:{STORE_PATH}?dir=..").as_str(),
        ] {
            assert!(
                check_flake_input_override_reference_is_remote_flake_or_single_store_path(
                    "nixpkgs", flake_ref
                )
                .is_err(),
                "{flake_ref}"
            );
        }
    }

    #[test]
    fn allowed_uri_prefixes_must_start_with_a_remote_scheme() {
        for prefix in [
            "github:",
            "https://example.com/archive",
            "git+ssh://git@h/x.git",
        ] {
            assert!(
                check_allowed_uri_prefix_has_remote_nix_scheme(prefix).is_ok(),
                "{prefix}"
            );
        }
        for prefix in [
            "path:/",
            "path:",
            "file:",
            "file:///etc",
            "/",
            "git+file:",
            "s3://b",
            "ftp://h",
            "channel:",
            "hg+file:",
            "PATH:/",
        ] {
            assert_eq!(
                check_allowed_uri_prefix_has_remote_nix_scheme(prefix).unwrap_err(),
                InputError::AllowedUriPrefixNotRemoteNixScheme(prefix.into()),
                "{prefix}"
            );
        }
    }

    fn lock_with_path_input(path: &str) -> Value {
        serde_json::json!({
            "nodes": {
                "root": { "inputs": { "s": "s" } },
                "s": {
                    "flake": false,
                    "locked": { "lastModified": 1, "narHash": NAR_HASH, "path": path, "type": "path" },
                    "original": { "path": path, "type": "path" },
                },
            },
            "root": "root",
            "version": 7,
        })
    }

    #[test]
    fn rejects_flake_lock_path_node_with_absolute_host_path_from_nix_only_flake_nix_url() {
        let lock = lock_with_path_input("/etc/machine-id");

        assert!(matches!(
            validate_flake_lock_computed_during_evaluation(&lock, None, &[]),
            Err(InputError::FlakeLockNodeValidationFailed { .. })
        ));
        assert!(
            validate_flake_lock_computed_during_evaluation(&lock, None, &["path:/etc/machine-id"])
                .is_err()
        );
    }

    #[test]
    fn an_absolute_path_node_equal_to_an_overridden_input_store_path_is_accepted() {
        let lock = lock_with_path_input(STORE_PATH);
        let reference = format!("path:{STORE_PATH}");

        validate_flake_lock_computed_during_evaluation(&lock, None, &[&reference]).unwrap();
        assert!(validate_flake_lock_computed_during_evaluation(&lock, None, &[]).is_err());
    }

    mod fetch_request_content_hash_store_path_tests {
        use super::*;

        const EXAMPLE_SOURCE_TREE_NAR_SHA256_SRI: &str =
            "sha256-OXhFkURii0pxIkZ2DeaOUN+tybxn+ccJvfDKIsA2UXk=";
        const EXAMPLE_SOURCE_TREE_NAR_SHA256_IN_NIX_BASE32: &str =
            "0yai6v025jphpl4wgyb7pk4svpshivk0sxj649qlm2v28j8lay1r";
        const EXAMPLE_SOURCE_TREE_NAR_SHA256_HEX: &str =
            "3978459144628b4a712246760de68e50dfadc9bc67f9c709bdf0ca22c0365179";
        const EXAMPLE_SINGLE_FILE_SHA256_HEX: &str =
            "7e4fa2eb8c7ac089739d5defc4489fad68a100d92082ca35c6b40a4524821f87";
        const EXAMPLE_FILE_SHA256_IN_SUBRESOURCE_INTEGRITY_FORMAT: &str =
            "sha256-fk+i64x6wIlznV3vxEifrWihANkggso1xrQKRSSCH4c=";

        const EXPECTED_STORE_PATH_OF_EXAMPLE_SOURCE_TREE_WITH_DEFAULT_NAME: &str =
            "/nix/store/ljaj8w6q4587ixa0yc41sw94m8a4qy8d-source";
        const EXPECTED_NIX_STORE_PATH_FOR_SOURCE_NAMED_NIXPKGS_SRC: &str =
            "/nix/store/6pxvdih063vg7jivjd63vhabh74y6s2w-nixpkgs-src";
        const EXPECTED_NIX_STORE_PATH_FOR_EXAMPLE_FILE_NAR_HASH_NAMED_SOURCE: &str =
            "/nix/store/hvxcd12h682ima3q8dwh15h4xa6idvfy-source";
        const EXPECTED_FETCHURL_FLAT_HASH_OTHER_TXT_STORE_PATH: &str =
            "/nix/store/0dqs3zalkrvpvplj3vhdacx2l3x6qdvf-other.txt";
        const EXPECTED_NIX_STORE_PATH_FOR_FETCHURL_FLAT_HASHING_NAMED_RENAMED: &str =
            "/nix/store/pn6g2snsv45j9gf119qjy38zf8bj8b07-renamed";

        fn parse_fetch_request_json_and_return_content_store_path(
            request: Value,
        ) -> Option<String> {
            AllowedFetchRequest::parse(&request.to_string())
                .expect("valid request")
                .pin()
        }

        fn build_fetch_request_json(builtin: &str, args: Value) -> Value {
            serde_json::json!({ "builtin": builtin, "args": args })
        }

        #[test]
        fn fetch_tree_with_nar_hash_gives_store_path_from_nar_hash_and_name_changes_path_suffix() {
            let github = |extra: Value| {
                let mut args = serde_json::json!({
                    "type": "github", "owner": "o", "repo": "r", "narHash": EXAMPLE_SOURCE_TREE_NAR_SHA256_SRI
                });
                args.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                serde_json::json!({ "builtin": "fetchTree", "args": args })
            };
            assert_eq!(
                parse_fetch_request_json_and_return_content_store_path(github(serde_json::json!(
                    {}
                )))
                .as_deref(),
                Some(EXPECTED_STORE_PATH_OF_EXAMPLE_SOURCE_TREE_WITH_DEFAULT_NAME)
            );
            assert_eq!(
                parse_fetch_request_json_and_return_content_store_path(github(
                    serde_json::json!({ "name": "nixpkgs-src" })
                ))
                .as_deref(),
                Some(EXPECTED_NIX_STORE_PATH_FOR_SOURCE_NAMED_NIXPKGS_SRC)
            );
        }

        #[test]
        fn fetch_tarball_with_sha256_returns_store_path_from_unpacked_nar_hash() {
            let url = "https://example.org/a.tar.gz";
            assert_eq!(
                parse_fetch_request_json_and_return_content_store_path(build_fetch_request_json(
                    "fetchTarball",
                    serde_json::json!({ "url": url, "sha256": EXAMPLE_SOURCE_TREE_NAR_SHA256_IN_NIX_BASE32 })
                ))
                .as_deref(),
                Some(EXPECTED_STORE_PATH_OF_EXAMPLE_SOURCE_TREE_WITH_DEFAULT_NAME)
            );
            assert_eq!(
                parse_fetch_request_json_and_return_content_store_path(build_fetch_request_json(
                    "fetchTarball",
                    serde_json::json!({
                        "url": url, "sha256": EXAMPLE_SOURCE_TREE_NAR_SHA256_HEX, "name": "nixpkgs-src"
                    })
                ))
                .as_deref(),
                Some(EXPECTED_NIX_STORE_PATH_FOR_SOURCE_NAMED_NIXPKGS_SRC)
            );
        }

        #[test]
        fn fetchurl_with_sha256_uses_flat_file_hash_for_store_path() {
            assert_eq!(
                parse_fetch_request_json_and_return_content_store_path(build_fetch_request_json(
                    "fetchurl",
                    serde_json::json!({
                        "url": "https://example.org/files/other.txt", "sha256": EXAMPLE_SINGLE_FILE_SHA256_HEX
                    })
                ))
                .as_deref(),
                Some(EXPECTED_FETCHURL_FLAT_HASH_OTHER_TXT_STORE_PATH)
            );
            assert_eq!(
                parse_fetch_request_json_and_return_content_store_path(build_fetch_request_json(
                    "fetchurl",
                    serde_json::json!({
                        "url": "https://example.org/files/other.txt",
                        "sha256": EXAMPLE_FILE_SHA256_IN_SUBRESOURCE_INTEGRITY_FORMAT,
                        "name": "renamed"
                    })
                ))
                .as_deref(),
                Some(EXPECTED_NIX_STORE_PATH_FOR_FETCHURL_FLAT_HASHING_NAMED_RENAMED)
            );
        }

        #[test]
        fn request_without_content_hash_has_no_computed_store_path_even_with_worker_store_path() {
            let unpinned = [
                tree(serde_json::json!({ "type": "github", "owner": "o", "repo": "r" })),
                serde_json::json!({
                    "builtin": "fetchTree",
                    "args": { "type": "github", "owner": "o", "repo": "r" },
                    "storePath": EXPECTED_STORE_PATH_OF_EXAMPLE_SOURCE_TREE_WITH_DEFAULT_NAME
                })
                .to_string(),
                build_fetch_request_json(
                    "fetchTarball",
                    serde_json::json!({ "url": "https://h/a.tar.gz" }),
                )
                .to_string(),
                build_fetch_request_json("fetchurl", serde_json::json!({ "url": "https://h/f" }))
                    .to_string(),
            ];
            for request in unpinned {
                assert_eq!(
                    AllowedFetchRequest::parse(&request).unwrap().pin(),
                    None,
                    "{request}"
                );
            }
        }

        #[test]
        fn test_hash_other_than_sha256_yields_no_store_path() {
            let sha512 = "sha512-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
            for hash in [sha512, "not-a-hash"] {
                assert_eq!(
                    parse_fetch_request_json_and_return_content_store_path(
                        build_fetch_request_json(
                            "fetchTarball",
                            serde_json::json!({ "url": "https://h/a.tar.gz", "sha256": hash })
                        )
                    ),
                    None
                );
            }
        }

        #[test]
        fn same_content_hash_from_different_fetch_sources_computes_same_store_path() {
            let pins = [
                build_fetch_request_json(
                    "fetchTarball",
                    serde_json::json!({ "url": "https://a.example/x.tar.gz", "sha256": EXAMPLE_SOURCE_TREE_NAR_SHA256_IN_NIX_BASE32 }),
                ),
                build_fetch_request_json(
                    "fetchTarball",
                    serde_json::json!({ "url": "https://b.example/y.tar.gz", "sha256": format!("sha256:{EXAMPLE_SOURCE_TREE_NAR_SHA256_HEX}") }),
                ),
                serde_json::json!({
                    "builtin": "fetchTree",
                    "args": {
                        "type": "github", "owner": "o", "repo": "r", "rev": "abc",
                        "narHash": EXAMPLE_SOURCE_TREE_NAR_SHA256_SRI
                    }
                }),
                serde_json::json!({
                    "builtin": "fetchTree",
                    "args": {
                        "type": "tarball", "url": "https://c.example/z.tar.gz",
                        "narHash": EXAMPLE_SOURCE_TREE_NAR_SHA256_SRI
                    }
                }),
            ]
            .map(parse_fetch_request_json_and_return_content_store_path);
            assert!(
                pins.iter().all(|pin| pin.as_deref()
                    == Some(EXPECTED_STORE_PATH_OF_EXAMPLE_SOURCE_TREE_WITH_DEFAULT_NAME)),
                "{pins:?}"
            );
        }

        #[test]
        fn fetch_requests_with_different_content_hashes_compute_different_store_paths() {
            let with_nar_hash = |hash: &str| {
                parse_fetch_request_json_and_return_content_store_path(build_fetch_request_json(
                    "fetchTree",
                    serde_json::json!({
                        "type": "github", "owner": "o", "repo": "r", "narHash": hash
                    }),
                ))
            };
            assert_eq!(
                with_nar_hash(EXAMPLE_SOURCE_TREE_NAR_SHA256_SRI).as_deref(),
                Some(EXPECTED_STORE_PATH_OF_EXAMPLE_SOURCE_TREE_WITH_DEFAULT_NAME)
            );
            assert_eq!(
                with_nar_hash(EXAMPLE_FILE_SHA256_IN_SUBRESOURCE_INTEGRITY_FORMAT).as_deref(),
                Some(EXPECTED_NIX_STORE_PATH_FOR_EXAMPLE_FILE_NAR_HASH_NAMED_SOURCE)
            );
        }
    }
}
