/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use gradient_eval::ipc::ExecApp;
use gradient_wire::types::{DeviceRequest, ExecSpec};
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
}

fn one_node() -> u32 {
    1
}

pub(crate) fn exec_spec(app: ExecApp) -> Result<ExecSpec, String> {
    spec_of(app).map_err(|e| format!("passthru.gradient: {e}"))
}

fn spec_of(app: ExecApp) -> Result<ExecSpec, String> {
    let block = nix_block(&app.block)?;
    if block.nodes == 0 {
        return Err("nodes must be at least 1".into());
    }
    if block.ports.contains(&0) {
        return Err("ports: port 0 cannot be forwarded".into());
    }

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
        sops: vec![],
        keep_running: block.keep_running,
        timeout_secs: block.timeout,
        max_silent_secs: block.max_silent,
        retries: block.retries,
    })
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
        }
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
