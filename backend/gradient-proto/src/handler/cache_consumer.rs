/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_core::upstream::inbound_cached_path;
use gradient_types::{StorePath, StorePathMapper};
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use gradient_util::http_validation::UserUrlDestinationAllowlist;
use gradient_wire::types::{CachedPath, GradientCapabilities, QueryMode};
use tracing::warn;

use gradient_wire::messages::{ClientMessage, ServerMessage};

const SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn proto_ws_url(base_url: &ValidatedHttpBaseUrl, remote_cache: &str) -> String {
    let base = base_url.as_str();
    let ws = match base.strip_prefix("http://") {
        Some(rest) => format!("ws://{rest}"),
        None => format!("wss://{}", base.trim_start_matches("https://")),
    };
    format!("{ws}/cache/{remote_cache}/proto")
}

fn consumer_capabilities() -> GradientCapabilities {
    GradientCapabilities {
        core: true,
        build: false,
        eval: false,
        network: false,
        secrets: false,
        exec: false,
        cache: true,
        federate: false,
    }
}

pub(crate) async fn pull_paths(
    base_url: &ValidatedHttpBaseUrl,
    remote_cache: &str,
    api_key: Option<&str>,
    allowlist: &UserUrlDestinationAllowlist,
    mapper: &StorePathMapper,
    paths: &[StorePath],
) -> Vec<CachedPath> {
    let addrs = match allowlist.allowed_socket_addresses_for_url(base_url).await {
        Ok(addrs) => addrs,
        Err(error) => {
            let host = base_url
                .as_str()
                .split_once("://")
                .and_then(|(_, rest)| rest.split('/').next())
                .unwrap_or_default();
            warn!(%host, remote_cache, %error, "Skip the gradient-proto upstream cache. The URL is not an allowed destination.");
            return vec![];
        }
    };
    let url = proto_ws_url(base_url, remote_cache);
    let mut socket =
        match gradient_wire::client::dial_with_prechecked_socket_addrs(&url, &addrs, api_key).await
        {
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
        paths: paths.iter().map(|p| mapper.outbound(p).full()).collect(),
        mode: QueryMode::Pull,
        nar_sizes: Vec::new(),
        external: false,
    };
    if writer.send_msg(&query).await.is_err() {
        return vec![];
    }

    match tokio::time::timeout(Duration::from_secs(30), reader.recv()).await {
        Ok(Some(ServerMessage::CacheStatus { cached, .. })) => cached
            .into_iter()
            .filter(|c| c.cached)
            .filter_map(|c| inbound_cached_path(mapper, c).ok())
            .collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::IdentityMapping;
    use gradient_types::ids::CacheId;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[allow(
        clippy::disallowed_methods,
        reason = "test listener for counting accepted connections"
    )]
    async fn loopback_listener_with_accept_counter() -> (u16, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        (port, accepted)
    }

    fn identity_mapper() -> StorePathMapper {
        StorePathMapper::for_cache(Arc::new(IdentityMapping), CacheId::nil())
    }

    fn does_localhost_with_trailing_dot_resolve() -> bool {
        use std::net::ToSocketAddrs;
        ("localhost.", 0)
            .to_socket_addrs()
            .is_ok_and(|mut addresses| addresses.next().is_some())
    }

    async fn pull_from_loopback_upstream(
        allowlist: &UserUrlDestinationAllowlist,
        upstream_host_names_or_ip_literals: &[&str],
    ) -> usize {
        let (port, accepted) = loopback_listener_with_accept_counter().await;
        for host in upstream_host_names_or_ip_literals {
            let base: ValidatedHttpBaseUrl =
                format!("http://{host}:{port}").parse().expect("base url");
            let found = pull_paths(
                &base,
                "prod",
                Some("key"),
                allowlist,
                &identity_mapper(),
                &[],
            )
            .await;
            assert!(found.is_empty());
        }
        accepted.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn empty_allow_list_opens_no_connection_to_a_loopback_upstream() {
        let accepted = pull_from_loopback_upstream(
            &UserUrlDestinationAllowlist::default(),
            &["localhost.", "127.0.0.1"],
        )
        .await;
        assert_eq!(accepted, 0);
    }

    #[tokio::test]
    async fn control_case_allow_list_with_loopback_range_connects_to_loopback_ip_literal_upstream_with_one_accepted_connection()
     {
        let allowlist =
            UserUrlDestinationAllowlist::parse(&["127.0.0.0/8".to_owned()]).expect("allowlist");
        assert_eq!(
            pull_from_loopback_upstream(&allowlist, &["127.0.0.1"]).await,
            1
        );
    }

    #[tokio::test]
    async fn control_case_allow_list_with_loopback_range_connects_to_upstream_named_localhost_resolving_to_loopback()
     {
        if !does_localhost_with_trailing_dot_resolve() {
            eprintln!("skipped: hostname localhost. cannot resolve on this host");
            return;
        }
        let allowlist =
            UserUrlDestinationAllowlist::parse(&["127.0.0.0/8".to_owned()]).expect("allowlist");
        assert_eq!(
            pull_from_loopback_upstream(&allowlist, &["localhost."]).await,
            1
        );
    }

    #[test]
    fn ws_url_from_https() {
        let ws_url = |base: &str, cache: &str| proto_ws_url(&base.parse().expect("base"), cache);
        assert_eq!(
            ws_url("https://remote.example", "prod"),
            "wss://remote.example/cache/prod/proto"
        );
        assert_eq!(
            ws_url("http://localhost:8080/", "dev"),
            "ws://localhost:8080/cache/dev/proto"
        );
    }
}
