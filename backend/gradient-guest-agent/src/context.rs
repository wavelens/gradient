/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::fmt;

use gradient_wire::guest::{ContextLayers, GuestSecret};

const OPEN: &str = "@gradient.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPlaceholder(pub String);

impl fmt::Display for UnknownPlaceholder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown placeholder @gradient.{}@", self.0)
    }
}

impl std::error::Error for UnknownPlaceholder {}

pub fn context_env(layers: &ContextLayers, secrets: &[GuestSecret]) -> BTreeMap<String, String> {
    let layer_entries = [&layers.worker, &layers.cluster, &layers.job]
        .into_iter()
        .flatten()
        .map(|(key, value)| (env_name("GRADIENT_", key), value.clone()));
    let secret_entries = secrets.iter().map(|secret| {
        (
            env_name("GRADIENT_SECRET_", &secret.name),
            secret_text(secret),
        )
    });
    layer_entries.chain(secret_entries).collect()
}

pub fn context_json(layers: &ContextLayers) -> serde_json::Value {
    serde_json::json!({
        "worker": layers.worker,
        "cluster": layers.cluster,
        "job": layers.job,
    })
}

pub fn resolve_placeholders(
    text: &str,
    layers: &ContextLayers,
    secrets: &[GuestSecret],
) -> Result<String, UnknownPlaceholder> {
    let mut resolved = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        resolved.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        match after_open.find('@').map(|end| &after_open[..end]) {
            Some(name) if is_placeholder_name(name) => {
                resolved.push_str(&lookup(name, layers, secrets)?);
                rest = &after_open[name.len() + 1..];
            }
            _ => {
                resolved.push('@');
                rest = &rest[start + 1..];
            }
        }
    }
    resolved.push_str(rest);
    Ok(resolved)
}

fn lookup(
    name: &str,
    layers: &ContextLayers,
    secrets: &[GuestSecret],
) -> Result<String, UnknownPlaceholder> {
    let unknown = || UnknownPlaceholder(name.to_owned());
    let (layer, key) = name.split_once('.').ok_or_else(unknown)?;
    let map = match layer {
        "worker" => &layers.worker,
        "cluster" => &layers.cluster,
        "job" => &layers.job,
        "secret" => {
            return secrets
                .iter()
                .find(|secret| secret.name == key)
                .map(secret_text)
                .ok_or_else(unknown);
        }
        _ => return Err(unknown()),
    };
    map.get(key).cloned().ok_or_else(unknown)
}

fn is_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn env_name(prefix: &str, key: &str) -> String {
    format!("{prefix}{}", key.to_ascii_uppercase().replace('-', "_"))
}

fn secret_text(secret: &GuestSecret) -> String {
    String::from_utf8_lossy(&secret.value).into_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use gradient_wire::guest::{ContextLayers, GuestSecret};

    use super::{UnknownPlaceholder, context_env, resolve_placeholders};

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn layers() -> ContextLayers {
        ContextLayers {
            worker: map(&[("zone", "fra1"), ("mount_datasets", "/mnt/datasets")]),
            cluster: map(&[("master_addr", "10.233.0.3"), ("master_port", "29500")]),
            job: map(&[("commit", "abc")]),
        }
    }

    fn secret(name: &str, value: &str) -> GuestSecret {
        GuestSecret {
            name: name.to_owned(),
            value: value.as_bytes().to_vec(),
        }
    }

    #[test]
    fn every_layer_key_becomes_a_gradient_variable() {
        let env = context_env(&layers(), &[secret("wandb-token", "s3cr3t")]);

        assert_eq!(env["GRADIENT_ZONE"], "fra1");
        assert_eq!(env["GRADIENT_MOUNT_DATASETS"], "/mnt/datasets");
        assert_eq!(env["GRADIENT_MASTER_PORT"], "29500");
        assert_eq!(env["GRADIENT_COMMIT"], "abc");
        assert_eq!(env["GRADIENT_SECRET_WANDB_TOKEN"], "s3cr3t");
    }

    #[test]
    fn placeholders_resolve_in_place() {
        let text = "--master @gradient.cluster.master_addr@:@gradient.cluster.master_port@";

        assert_eq!(
            resolve_placeholders(text, &layers(), &[]),
            Ok("--master 10.233.0.3:29500".to_owned())
        );
    }

    #[test]
    fn an_unknown_key_fails_with_its_name() {
        let error =
            resolve_placeholders("@gradient.job.branch@", &layers(), &[]).expect_err("unknown key");

        assert_eq!(error, UnknownPlaceholder("job.branch".to_owned()));
        assert_eq!(
            error.to_string(),
            "unknown placeholder @gradient.job.branch@"
        );
    }

    #[test]
    fn text_without_a_closing_at_stays_as_it_is() {
        assert_eq!(
            resolve_placeholders("mail@gradient.dev", &layers(), &[]),
            Ok("mail@gradient.dev".to_owned())
        );
    }

    #[test]
    fn a_secret_value_holding_a_placeholder_is_not_expanded_again() {
        let secrets = [secret("tricky", "@gradient.job.commit@")];

        assert_eq!(
            resolve_placeholders("@gradient.secret.tricky@", &layers(), &secrets),
            Ok("@gradient.job.commit@".to_owned())
        );
    }
}
