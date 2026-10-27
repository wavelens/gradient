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
    pub ciphertext: Vec<u8>,
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
