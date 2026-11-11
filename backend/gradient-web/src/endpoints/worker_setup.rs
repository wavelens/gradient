/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::endpoints::projects::workers::RegisterWorkerRequest;
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerRegistrationResponse {
    pub id: String,
    pub name: String,
    pub url: String,
    /// Project ID or team ID that the worker names in its peers file. The peers line is `<peer_id>:<token>`.
    pub peer_id: String,
    /// One-time token for the worker. The server can save only the hash and can't show the token again.
    pub token: String,
    pub nixos_configuration: String,
}

impl WorkerRegistrationResponse {
    pub fn new(
        serve_url: &str,
        body: &RegisterWorkerRequest,
        url: String,
        peer_id: impl ToString,
        token: String,
    ) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        Self {
            nixos_configuration: render_new_worker_nixos_configuration_snippet(
                serve_url, &id, body,
            ),
            id,
            name: body.display_name.trim().to_string(),
            url,
            peer_id: peer_id.to_string(),
            token,
        }
    }
}

const CAPABILITIES: [&str; 5] = ["eval", "build", "network", "secrets", "exec"];

fn proto_url(serve_url: &str) -> String {
    let base = serve_url.trim_end_matches('/');
    let websocket = match base.split_once("://") {
        Some(("http", host)) => format!("ws://{host}"),
        Some((_, host)) => format!("wss://{host}"),
        None => format!("wss://{base}"),
    };
    format!("{websocket}/proto")
}

pub fn render_new_worker_nixos_configuration_snippet(
    serve_url: &str,
    worker_id: &str,
    body: &RegisterWorkerRequest,
) -> String {
    let enabled = [
        body.enable_eval,
        body.enable_build,
        body.enable_network,
        body.enable_secrets,
        body.enable_exec,
    ];
    let capabilities: String = CAPABILITIES
        .iter()
        .zip(enabled)
        .map(|(name, on)| format!("    {name} = {on};\n"))
        .collect();
    format!(
        "services.gradient.worker = {{\n  enable = true;\n  id = \"{worker_id}\";\n  serverUrl = \"{}\";\n  # Replace this with the path to the file for the peers line below.\n  peersFile = \"/run/secrets/gradient-worker-peers\";\n  capabilities = {{\n{capabilities}  }};\n}};\n",
        proto_url(serve_url)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn request() -> RegisterWorkerRequest {
        serde_json::from_value(serde_json::json!({ "display_name": "Builder" })).unwrap()
    }

    #[test]
    fn the_snippet_uses_option_names_of_the_worker_module() {
        let options: Vec<Value> =
            serde_json::from_str(include_str!("../../../gradient-worker/options.json")).unwrap();
        let declared = |group: &str, field: &str| {
            options
                .iter()
                .any(|option| option["group"] == group && option["field"] == field)
        };
        for (group, field) in [("", "id"), ("", "server_url"), ("", "peers_file")] {
            assert!(declared(group, field), "{field} is a worker option");
        }
        for capability in CAPABILITIES {
            assert!(
                declared("capabilities", capability),
                "{capability} is a capability option"
            );
        }

        let snippet = render_new_worker_nixos_configuration_snippet(
            "https://ci.example.com/",
            "worker-id",
            &request(),
        );
        for line in [
            "id = \"worker-id\";",
            "serverUrl = \"wss://ci.example.com/proto\";",
            "peersFile = ",
            "exec = false;",
            "eval = true;",
        ] {
            assert!(snippet.contains(line), "{line} in {snippet}");
        }
    }

    #[test]
    fn a_plain_http_server_gets_an_unencrypted_websocket_url() {
        assert_eq!(
            proto_url("http://localhost:3000"),
            "ws://localhost:3000/proto"
        );
    }
}
