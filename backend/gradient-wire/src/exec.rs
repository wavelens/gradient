/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::fmt;

use crate::codec::Proto;
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct DeviceRequest {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeviceRequestError {
    #[error("device name is empty")]
    Empty,
    #[error("device name has an empty part before or after a colon")]
    EmptyPart,
    #[error("device count \"{0}\" is out of range")]
    BadCount(String),
    #[error("device count must be at least 1")]
    ZeroCount,
}

impl DeviceRequest {
    pub fn parse(s: &str) -> Result<DeviceRequest, DeviceRequestError> {
        let (name, count) = match s.rsplit_once(':') {
            Some((name, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                let count = tail
                    .parse::<u32>()
                    .map_err(|_| DeviceRequestError::BadCount(tail.to_owned()))?;
                (name, count)
            }
            _ => (s, 1),
        };
        if name.is_empty() {
            return Err(DeviceRequestError::Empty);
        }
        if name.split(':').any(str::is_empty) {
            return Err(DeviceRequestError::EmptyPart);
        }
        if count == 0 {
            return Err(DeviceRequestError::ZeroCount);
        }
        Ok(DeviceRequest {
            name: name.to_owned(),
            count,
        })
    }
}

impl fmt::Display for DeviceRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.count {
            1 => f.write_str(&self.name),
            count => write!(f, "{}:{count}", self.name),
        }
    }
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct ExecRequirement {
    pub architecture: String,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub mounts: Vec<String>,
    pub network: bool,
    pub ssh: bool,
    pub ports: Vec<u16>,
    pub custom_image: bool,
    #[proto(45, default)]
    #[serde(default)]
    pub secrets: Vec<String>,
    #[proto(45, default)]
    #[serde(default)]
    pub server_secrets: Vec<String>,
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct DeviceClass {
    pub name: String,
    pub total: u32,
    pub free: u32,
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct ExecAllow {
    pub network: bool,
    pub ssh: bool,
    pub ports: (u16, u16),
    pub nodes: u32,
    pub devices: Vec<String>,
    pub mounts: Vec<String>,
    pub custom_images: bool,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub retries: u32,
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct ExecDefaults {
    pub network: bool,
    pub ssh: bool,
    pub mounts: Vec<String>,
    pub max_silent_secs: Option<u64>,
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct ExecInventory {
    pub labels: Vec<String>,
    pub devices: Vec<DeviceClass>,
    pub mounts: Vec<String>,
    pub secrets: Vec<String>,
    pub allow: ExecAllow,
    pub defaults: ExecDefaults,
    pub warm_guests: u32,
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct Phase {
    pub name: String,
    pub payload: Vec<u8>,
}

#[derive(Proto, Debug, Clone, PartialEq, Default)]
pub struct JobContext {
    pub job: String,
    pub attempt: String,
    pub evaluation: String,
    pub task: String,
    pub project: String,
    pub repository: String,
    pub commit: String,
    pub entry_point: String,
    pub last_phase: Option<Phase>,
    pub api_url: String,
    pub token: String,
    #[proto(47, default)]
    pub parent_job: Option<String>,
    #[proto(47, default)]
    pub project_name: String,
    #[proto(47, default)]
    pub task_name: String,
}

#[derive(Proto, Debug, Clone, PartialEq)]
pub struct ExecJob {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub image: Option<String>,
    pub requirement: ExecRequirement,
    pub context: JobContext,
    pub secrets: Vec<String>,
    pub keep_running: bool,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub attempt: u32,
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct SopsFile {
    pub path: String,
    #[serde(with = "base64_bytes")]
    #[schema(value_type = String, format = Byte)]
    pub ciphertext: Vec<u8>,
}

mod base64_bytes {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(D::Error::custom)
    }
}

const MAX_SECRET_NAME: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("secret name \"{0}\": use 1 to 64 lowercase letters, digits or dashes")]
pub struct SecretNameError(pub String);

pub fn validate_secret_name(name: &str) -> Result<(), SecretNameError> {
    let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
    let valid = (1..=MAX_SECRET_NAME).contains(&name.len())
        && name.bytes().all(allowed)
        && !name.starts_with('-');
    if valid {
        Ok(())
    } else {
        Err(SecretNameError(name.to_owned()))
    }
}

#[derive(
    Proto, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq, Default, utoipa::ToSchema,
)]
pub struct ExecSpec {
    pub program: String,
    pub nodes: u32,
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub network: Option<bool>,
    pub mounts: Option<Vec<String>>,
    pub image: Option<String>,
    pub ssh: Option<bool>,
    pub ports: Vec<u16>,
    pub env: BTreeMap<String, String>,
    pub args: Vec<String>,
    pub secrets: Vec<String>,
    pub sops: Vec<SopsFile>,
    pub keep_running: bool,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub retries: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecLimits {
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub retries: u32,
}

impl ExecSpec {
    pub fn requirement_on(&self, architecture: &str, defaults: &ExecDefaults) -> ExecRequirement {
        ExecRequirement {
            architecture: architecture.to_owned(),
            labels: self.labels.clone(),
            devices: self.devices.clone(),
            mounts: self
                .mounts
                .clone()
                .unwrap_or_else(|| defaults.mounts.clone()),
            network: self.network.unwrap_or(defaults.network),
            ssh: self.ssh.unwrap_or(defaults.ssh),
            ports: self.ports.clone(),
            custom_image: self.image.is_some(),
            secrets: Vec::new(),
            server_secrets: Vec::new(),
        }
    }

    pub fn limits_on(&self, allow: &ExecAllow, defaults: &ExecDefaults) -> ExecLimits {
        ExecLimits {
            timeout_secs: capped(self.timeout_secs, allow.timeout_secs),
            max_silent_secs: capped(
                self.max_silent_secs.or(defaults.max_silent_secs),
                allow.max_silent_secs,
            ),
            retries: self.retries.min(allow.retries),
        }
    }
}

fn capped(value: Option<u64>, cap: Option<u64>) -> Option<u64> {
    match (value, cap) {
        (Some(value), Some(cap)) => Some(value.min(cap)),
        (value, cap) => value.or(cap),
    }
}

pub const MAX_PHASE_PAYLOAD: usize = 65536;

pub const TREE_PATCH_OUTPUT: &str = "gradient/tree.patch";
pub const CONTINUE_OUTPUT: &str = "gradient/continue.json";
pub const MAX_CONTINUE_FILE_BYTES: usize = 65536;
pub const MAX_OUTPUT_PATH_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputPathError {
    #[error("output path is empty")]
    Empty,
    #[error("output path is longer than {MAX_OUTPUT_PATH_BYTES} bytes")]
    TooLong,
    #[error("output path \"{0}\": the path must be relative")]
    Absolute(String),
    #[error("output path \"{0}\": empty, \".\" or \"..\" segments and NUL bytes are not allowed")]
    BadSegment(String),
}

pub fn check_output_path(path: &str) -> Result<(), OutputPathError> {
    if path.is_empty() {
        return Err(OutputPathError::Empty);
    }
    if path.len() > MAX_OUTPUT_PATH_BYTES {
        return Err(OutputPathError::TooLong);
    }
    if path.starts_with('/') {
        return Err(OutputPathError::Absolute(path.to_owned()));
    }
    let bad_segment = |segment: &str| matches!(segment, "" | "." | "..") || segment.contains('\0');
    if path.split('/').any(bad_segment) {
        return Err(OutputPathError::BadSegment(path.to_owned()));
    }
    Ok(())
}

pub fn exec_image_attr(attr: &str) -> String {
    format!("{attr}.passthru.gradient.image")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_string_names_a_count_that_defaults_to_one() {
        assert_eq!(
            DeviceRequest::parse("gpu:h100:8").unwrap(),
            DeviceRequest {
                name: "gpu:h100".into(),
                count: 8
            }
        );
        assert_eq!(
            DeviceRequest::parse("kvm").unwrap(),
            DeviceRequest {
                name: "kvm".into(),
                count: 1
            }
        );
        assert_eq!(
            DeviceRequest::parse("gpu:h100").unwrap(),
            DeviceRequest {
                name: "gpu:h100".into(),
                count: 1
            }
        );
        assert!(matches!(
            DeviceRequest::parse("gpu:0"),
            Err(DeviceRequestError::ZeroCount)
        ));
        assert!(matches!(
            DeviceRequest::parse(""),
            Err(DeviceRequestError::Empty)
        ));
    }

    #[test]
    fn a_device_name_with_an_empty_part_is_refused() {
        for device in ["gpu:", "gpu::8", ":h100", "gpu::h100"] {
            assert!(
                matches!(
                    DeviceRequest::parse(device),
                    Err(DeviceRequestError::EmptyPart)
                ),
                "{device}"
            );
        }
    }

    #[test]
    fn defaults_fill_only_what_the_app_left_out() {
        let defaults = ExecDefaults {
            network: false,
            ssh: true,
            mounts: vec!["scratch".into()],
            max_silent_secs: Some(3600),
        };
        let spec = ExecSpec {
            network: Some(true),
            ..ExecSpec::default()
        };
        let req = spec.requirement_on("x86_64-linux", &defaults);
        assert!(req.network);
        assert!(req.ssh);
        assert_eq!(req.mounts, vec!["scratch".to_owned()]);
        let explicit = ExecSpec {
            mounts: Some(vec![]),
            ssh: Some(false),
            ..ExecSpec::default()
        };
        let req = explicit.requirement_on("x86_64-linux", &defaults);
        assert!(req.mounts.is_empty() && !req.ssh);
    }

    #[test]
    fn limits_are_clamped_to_the_worker_caps() {
        let allow = ExecAllow {
            timeout_secs: Some(30 * 24 * 3600),
            max_silent_secs: Some(24 * 3600),
            retries: 5,
            ..ExecAllow::default()
        };
        let defaults = ExecDefaults {
            max_silent_secs: Some(3600),
            ..ExecDefaults::default()
        };
        let spec = ExecSpec {
            timeout_secs: Some(60 * 24 * 3600),
            retries: 9,
            ..ExecSpec::default()
        };
        assert_eq!(
            spec.limits_on(&allow, &defaults),
            ExecLimits {
                timeout_secs: Some(30 * 24 * 3600),
                max_silent_secs: Some(3600),
                retries: 5
            }
        );
        let open = ExecSpec::default();
        assert_eq!(
            open.limits_on(&ExecAllow::default(), &ExecDefaults::default()),
            ExecLimits {
                timeout_secs: None,
                max_silent_secs: None,
                retries: 0
            }
        );
    }

    #[test]
    fn secret_names_are_lowercase_letters_digits_and_dashes() {
        for ok in ["wandb-token", "hf2", "a"] {
            assert!(validate_secret_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Wandb", "a_b", "-x", &"a".repeat(65)] {
            assert!(validate_secret_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sops_ciphertext_is_base64_in_json() {
        let file = SopsFile {
            path: "a.yaml".into(),
            ciphertext: b"abc".to_vec(),
        };
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(json["ciphertext"], "YWJj");
        assert_eq!(serde_json::from_value::<SopsFile>(json).unwrap(), file);
    }

    #[test]
    fn an_output_path_stays_inside_the_job() {
        for ok in ["model/weights.bin", "gradient/continue.json", "a.txt"] {
            assert!(check_output_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a/",
            "./a",
            "a\0b",
        ] {
            assert!(check_output_path(bad).is_err(), "{bad:?}");
        }
        assert!(matches!(
            check_output_path(&"a".repeat(MAX_OUTPUT_PATH_BYTES + 1)),
            Err(OutputPathError::TooLong)
        ));
    }

    #[test]
    fn an_env_map_round_trips_in_order() {
        let env = BTreeMap::from([
            ("B".to_owned(), "2".to_owned()),
            ("A".to_owned(), "1".to_owned()),
        ]);
        let back: BTreeMap<String, String> =
            crate::codec::from_bytes(crate::codec::to_bytes(&env, 0).unwrap(), 0).unwrap();
        assert_eq!(back, env);
    }
}
