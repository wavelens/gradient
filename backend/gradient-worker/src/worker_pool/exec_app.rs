/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet};

use gradient_eval::ipc::ExecApp;
use gradient_wire::types::{DeviceRequest, ExecSpec, GuestSize, SopsFile, validate_secret_name};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NixBlock {
    #[serde(default = "one_node")]
    nodes: u32,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    devices: Vec<String>,
    network: Option<bool>,
    mounts: Option<Vec<String>>,
    ssh: Option<bool>,
    #[serde(default)]
    ports: Vec<u16>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    secrets: Vec<String>,
    #[serde(default)]
    keep_running: bool,
    timeout: Option<u64>,
    max_silent: Option<u64>,
    #[serde(default)]
    retries: u32,
    cpus: Option<u32>,
    memory_mb: Option<u64>,
    disk_mb: Option<u64>,
}

fn one_node() -> u32 {
    1
}

impl NixBlock {
    fn guest(&self) -> Result<GuestSize, String> {
        let default = GuestSize::default_request();
        let size = GuestSize {
            cpus: self.cpus.unwrap_or(default.cpus),
            memory_mb: self.memory_mb.unwrap_or(default.memory_mb),
            disk_mb: self.disk_mb.unwrap_or(default.disk_mb),
        };
        if !(1..=255).contains(&size.cpus) {
            return Err("cpus must be between 1 and 255".into());
        }
        if size.memory_mb < 128 {
            return Err("memoryMb must be at least 128".into());
        }
        if size.disk_mb < 64 {
            return Err("diskMb must be at least 64".into());
        }
        Ok(size)
    }
}

pub(crate) fn exec_spec(app: ExecApp) -> Result<ExecSpec, String> {
    spec_of(app).map_err(|e| format!("passthru.gradient: {e}"))
}

fn spec_of(app: ExecApp) -> Result<ExecSpec, String> {
    let block = nix_block(&app.block)?;
    if block.nodes == 0 {
        return Err("nodes must be at least 1".into());
    }
    if block.nodes > 1 && block.network == Some(false) {
        return Err("nodes > 1 needs network".into());
    }
    if block.ports.contains(&0) {
        return Err("ports: port 0 cannot be forwarded".into());
    }
    check_secret_names(&block.secrets)?;
    let guest = block.guest()?;

    Ok(ExecSpec {
        program: app.program,
        nodes: block.nodes,
        labels: block.labels,
        devices: devices(&block.devices)?,
        network: block.network,
        mounts: block.mounts,
        image: app.image_drv,
        ssh: block.ssh,
        ports: block.ports,
        env: block.env,
        args: block.args,
        secrets: block.secrets,
        sops: app
            .sops
            .into_iter()
            .map(|file| SopsFile {
                path: file.path,
                ciphertext: file.content.into_bytes(),
            })
            .collect(),
        keep_running: block.keep_running,
        timeout_secs: block.timeout,
        max_silent_secs: block.max_silent,
        retries: block.retries,
        guest,
    })
}

fn check_secret_names(names: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for name in names {
        validate_secret_name(name).map_err(|e| format!("secrets: {e}"))?;
        if !seen.insert(name) {
            return Err(format!("secrets: \"{name}\" is listed twice"));
        }
    }

    Ok(())
}

fn nix_block(json: &str) -> Result<NixBlock, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    serde_path_to_error::deserialize(value).map_err(|e| match e.path().to_string().as_str() {
        "." => e.inner().to_string(),
        path => format!("{path}: {}", e.inner()),
    })
}

fn devices(requests: &[String]) -> Result<Vec<DeviceRequest>, String> {
    requests
        .iter()
        .map(|s| DeviceRequest::parse(s).map_err(|e| format!("device \"{s}\": {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(block: &str) -> ExecApp {
        ExecApp {
            block: block.into(),
            program: "/nix/store/aaaa-train/bin/train".into(),
            program_drv: "aaaa-train.drv".into(),
            image_drv: None,
            sops: vec![],
        }
    }

    #[test]
    fn a_bad_secret_name_names_itself() {
        assert!(
            exec_spec(app(r#"{"secrets":["Bad_Name"]}"#))
                .unwrap_err()
                .contains("Bad_Name")
        );
        assert!(
            exec_spec(app(r#"{"secrets":["a1b2c3d4","a1b2c3d4"]}"#))
                .unwrap_err()
                .contains("a1b2c3d4")
        );
    }

    #[test]
    fn sops_files_reach_the_spec_as_bytes() {
        let mut with_sops = app("{}");
        with_sops.sops = vec![gradient_eval::ipc::ExecAppFile {
            path: "secrets/train.yaml".into(),
            content: "wandb-token: ENC[x]".into(),
        }];
        let spec = exec_spec(with_sops).unwrap();
        assert_eq!(
            spec.sops,
            vec![SopsFile {
                path: "secrets/train.yaml".into(),
                ciphertext: b"wandb-token: ENC[x]".to_vec()
            }]
        );
    }

    #[test]
    fn the_nix_names_map_to_the_spec() {
        let spec = exec_spec(app(
            r#"{"nodes":1,"devices":["gpu:h100:8","kvm"],"keepRunning":true,"timeout":1209600,"maxSilent":3600,"retries":2,"env":{"WANDB_PROJECT":"llm"},"ports":[6006]}"#,
        ))
        .unwrap();
        assert_eq!(
            spec.devices,
            vec![
                DeviceRequest {
                    name: "gpu:h100".into(),
                    count: 8
                },
                DeviceRequest {
                    name: "kvm".into(),
                    count: 1
                }
            ]
        );
        assert!(spec.keep_running);
        assert_eq!(
            (spec.timeout_secs, spec.max_silent_secs, spec.retries),
            (Some(14 * 24 * 3600), Some(3600), 2)
        );
        assert_eq!(spec.program, "/nix/store/aaaa-train/bin/train");
        assert_eq!(spec.network, None);
    }

    #[test]
    fn the_guest_size_defaults_to_two_cpus_and_500_mb() {
        assert_eq!(
            exec_spec(app("{}")).unwrap().guest,
            GuestSize {
                cpus: 2,
                memory_mb: 500,
                disk_mb: 2048
            }
        );
    }

    #[test]
    fn a_guest_size_out_of_range_names_its_key() {
        for (block, message) in [
            (r#"{"cpus":0}"#, "cpus must be between 1 and 255"),
            (r#"{"cpus":256}"#, "cpus must be between 1 and 255"),
            (r#"{"memoryMb":64}"#, "memoryMb must be at least 128"),
            (r#"{"diskMb":1}"#, "diskMb must be at least 64"),
        ] {
            let error = exec_spec(app(block)).unwrap_err();
            assert!(error.contains(message), "{block}: {error}");
        }
    }

    #[test]
    fn a_multi_node_app_cannot_turn_the_network_off() {
        assert!(
            exec_spec(app(r#"{"nodes":2,"network":false}"#))
                .unwrap_err()
                .contains("nodes > 1 needs network")
        );
        assert_eq!(exec_spec(app(r#"{"nodes":2}"#)).unwrap().nodes, 2);
    }

    #[test]
    fn a_bad_field_names_itself() {
        assert!(
            exec_spec(app(r#"{"devices":["gpu:0"]}"#))
                .unwrap_err()
                .contains("gpu:0")
        );
        assert!(
            exec_spec(app(r#"{"nodes":0}"#))
                .unwrap_err()
                .contains("nodes")
        );
        assert!(
            exec_spec(app(r#"{"netwrok":true}"#))
                .unwrap_err()
                .contains("netwrok")
        );
        assert!(
            exec_spec(app(r#"{"ports":[0]}"#))
                .unwrap_err()
                .contains("ports")
        );
        assert!(
            exec_spec(app(r#"{"network":"yes"}"#))
                .unwrap_err()
                .starts_with("passthru.gradient: network: ")
        );
        assert!(
            exec_spec(app(r#"{"env":{"X":1}}"#))
                .unwrap_err()
                .contains("env.X")
        );
    }
}
