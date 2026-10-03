/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_wire::types::{CachedPath, GradientCapabilities, QueryMode};

use gradient_wire::messages::{ClientMessage, ServerMessage};

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn proto_ws_url(base_url: &str, remote_cache: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let ws = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("wss://{trimmed}")
    };
    format!("{ws}/cache/{remote_cache}/proto")
}

fn consumer_capabilities() -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        build: false,
        eval: false,
        fetch: false,
        cache: true,
        federate: false,
    }
}

pub(crate) async fn pull_paths(
    base_url: &str,
    remote_cache: &str,
    api_key: Option<&str>,
    paths: &[String],
) -> Vec<CachedPath> {
    let url = proto_ws_url(base_url, remote_cache);
    let mut socket = match gradient_wire::client::dial_with_auth(&url, api_key).await {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let init = ClientMessage::InitConnection {
        capabilities: consumer_capabilities(),
        id: uuid::Uuid::now_v7().to_string(),
    };
    let sent = tokio::time::timeout(Duration::from_secs(10), socket.send_client_msg(&init)).await;
    if !matches!(sent, Ok(Ok(()))) {
        return vec![];
    }

    match tokio::time::timeout(Duration::from_secs(10), socket.recv_server_msg()).await {
        Ok(Some(ServerMessage::InitAck { .. })) => {}
        _ => return vec![],
    }

    let (mut reader, writer, _writer_task) = socket.split_peer(SEND_TIMEOUT);
    let query = ClientMessage::CacheQuery {
        job_id: uuid::Uuid::now_v7().to_string(),
        query_id: uuid::Uuid::now_v7().to_string(),
        paths: paths.to_vec(),
        mode: QueryMode::Pull,
        nar_sizes: Vec::new(),
        external: false,
    };
    if writer.send_msg(&query).await.is_err() {
        return vec![];
    }

    match tokio::time::timeout(Duration::from_secs(30), reader.recv()).await {
        Ok(Some(ServerMessage::CacheStatus { cached, .. })) => {
            cached.into_iter().filter(|c| c.cached).collect()
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_from_https() {
        assert_eq!(
            proto_ws_url("https://remote.example", "prod"),
            "wss://remote.example/cache/prod/proto"
        );
        assert_eq!(
            proto_ws_url("http://localhost:8080/", "dev"),
            "ws://localhost:8080/cache/dev/proto"
        );
        assert_eq!(
            proto_ws_url("remote.example", "x"),
            "wss://remote.example/cache/x/proto"
        );
    }
}
