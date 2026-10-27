/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use gradient_wire::guest::{GuestRelay, GuestRelayKind};

const STATE_FILE: &str = "state.json";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GuestId(String);

impl GuestId {
    pub fn new() -> Self {
        Self(Uuid::now_v7().simple().to_string())
    }
}

impl Default for GuestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GuestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for GuestId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid {
            return Err(format!("not a guest id: {s:?}"));
        }
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for GuestId {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<GuestId> for String {
    fn from(id: GuestId) -> Self {
        id.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldDevice {
    pub class: String,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRelay {
    pub name: String,
    pub port: u32,
    pub tcp_listen: Option<u16>,
}

impl From<&GuestRelay> for StoredRelay {
    fn from(relay: &GuestRelay) -> Self {
        let tcp_listen = match relay.kind {
            GuestRelayKind::Pty => None,
            GuestRelayKind::Tcp { listen } => Some(listen),
        };
        Self {
            name: relay.name.clone(),
            port: relay.port,
            tcp_listen,
        }
    }
}

impl From<&StoredRelay> for GuestRelay {
    fn from(relay: &StoredRelay) -> Self {
        let kind = match relay.tcp_listen {
            None => GuestRelayKind::Pty,
            Some(listen) => GuestRelayKind::Tcp { listen },
        };
        Self {
            name: relay.name.clone(),
            port: relay.port,
            kind,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuestState {
    pub guest_id: GuestId,
    pub unit: String,
    pub cid: u32,
    pub api_socket: PathBuf,
    pub address: Option<String>,
    pub tap: Option<String>,
    pub job: Option<serde_json::Value>,
    pub devices_held: Vec<HeldDevice>,
    pub mounts: Vec<String>,
    #[serde(default)]
    pub relays: Vec<StoredRelay>,
    #[serde(default)]
    pub usb_bus_ids: Vec<String>,
    #[serde(default)]
    pub per_job_dirs: Vec<PathBuf>,
}

impl GuestState {
    pub fn job_as<T: DeserializeOwned>(&self) -> Option<Result<T>> {
        self.job
            .as_ref()
            .map(|job| serde_json::from_value(job.clone()).context("job record in the guest state"))
    }
}

pub fn write_state(dir: &Path, state: &GuestState) -> Result<()> {
    let path = dir.join(STATE_FILE);
    let temp = dir.join(format!("{STATE_FILE}.tmp"));
    let bytes = serde_json::to_vec_pretty(state)?;
    std::fs::write(&temp, bytes).with_context(|| format!("write {}", temp.display()))?;
    std::fs::rename(&temp, &path).with_context(|| format!("rename to {}", path.display()))
}

pub fn read_states(guests_dir: &Path) -> Vec<(GuestId, Result<GuestState>)> {
    let Ok(entries) = std::fs::read_dir(guests_dir) else {
        return Vec::new();
    };
    let mut states: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_str()?.parse::<GuestId>().ok()?;
            Some((id, read_state(&entry.path())))
        })
        .collect();
    states.sort_by(|a, b| a.0.cmp(&b.0));
    states
}

fn read_state(dir: &Path) -> Result<GuestState> {
    let path = dir.join(STATE_FILE);
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let state: GuestState =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    if dir.file_name().and_then(|name| name.to_str()) != Some(&state.guest_id.0) {
        bail!("{} belongs to guest {}", path.display(), state.guest_id);
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy_state(id: GuestId) -> GuestState {
        GuestState {
            unit: format!("gradient-guest-{id}.service"),
            api_socket: PathBuf::from(format!("/var/lib/gradient-worker/guests/{id}/api.sock")),
            guest_id: id,
            cid: 3,
            address: Some("10.233.0.3".into()),
            tap: Some("gr-tap-3".into()),
            job: Some(serde_json::json!({ "job_id": "j1", "attempt": 2 })),
            devices_held: vec![
                HeldDevice {
                    class: "gpu:h100".into(),
                    unit: "vfio:0000:17:00.0".into(),
                },
                HeldDevice {
                    class: "gpu:h100".into(),
                    unit: "vfio:0000:18:00.0".into(),
                },
            ],
            mounts: vec!["datasets".into()],
            relays: Vec::new(),
            usb_bus_ids: vec!["1-2".into()],
            per_job_dirs: Vec::new(),
        }
    }

    fn guest_dir(root: &Path, id: &GuestId) -> PathBuf {
        let dir = root.join(id.to_string());
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn a_busy_guest_survives_a_write_and_read() {
        let root = tempfile::tempdir().unwrap();
        let id = GuestId::new();
        let state = busy_state(id.clone());
        write_state(&guest_dir(root.path(), &id), &state).unwrap();

        let mut read = read_states(root.path());
        assert_eq!(read.len(), 1);
        let (read_id, read_state) = read.remove(0);
        assert_eq!(read_id, id);
        assert_eq!(read_state.unwrap(), state);
    }

    #[test]
    fn a_state_from_an_older_worker_reads_with_empty_lists() {
        let root = tempfile::tempdir().unwrap();
        let id = GuestId::new();
        let dir = guest_dir(root.path(), &id);
        let old = serde_json::json!({
            "guest_id": id.to_string(),
            "unit": format!("gradient-guest-{id}.service"),
            "cid": 4,
            "api_socket": dir.join("api.sock"),
            "address": null,
            "tap": null,
            "job": null,
            "devices_held": [],
            "mounts": [],
        });
        std::fs::write(dir.join(STATE_FILE), old.to_string()).unwrap();

        let state = read_states(root.path()).remove(0).1.unwrap();
        assert!(state.relays.is_empty());
        assert!(state.usb_bus_ids.is_empty());
        assert_eq!(state.cid, 4);
    }

    #[test]
    fn a_directory_without_a_state_file_is_reported_unreadable() {
        let root = tempfile::tempdir().unwrap();
        let id = GuestId::new();
        guest_dir(root.path(), &id);
        std::fs::create_dir(root.path().join("not-a-guest")).unwrap();

        let read = read_states(root.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, id);
        assert!(read[0].1.is_err());
    }
}
