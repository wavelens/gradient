/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::OnceLock;
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

const DOWNLOAD_MAX_REDIRECTS: usize = 5;

pub fn user_agent() -> String {
    format!(
        "Gradient/{} (+https://github.com/wavelens/gradient)",
        env!("CARGO_PKG_VERSION")
    )
}

/// rustls 0.23 is refusing to pick a provider when zero or several are enabled. A TLS handshake
/// before installation is panicking. Binaries must call this before any code path opens a TLS
/// connection. A second call is returning `Err`, and ignoring it is deliberate.
pub fn init_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

fn rustls_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

fn rustls_config(alpn: &[&[u8]]) -> rustls::ClientConfig {
    init_crypto_provider();
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls_root_store())
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();

    config
}

const NEGOTIATED: &[&[u8]] = &[b"h2", b"http/1.1"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpVersion {
    Http1,
    Http2,
}

pub fn untimed_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(user_agent())
        .use_preconfigured_tls(rustls_config(NEGOTIATED))
        .http2_adaptive_window(true)
}

fn client_builder() -> reqwest::ClientBuilder {
    untimed_client_builder().timeout(DEFAULT_TIMEOUT)
}

pub fn build_client() -> reqwest::Result<reqwest::Client> {
    client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Attic, Cachix and S3 gateways are answering a NAR GET with a 3xx to their object storage.
/// reqwest is reporting an unfollowed 3xx as a successful empty body. Object GETs are carrying no
/// credentials worth leaking through a redirect.
pub fn build_download_client() -> reqwest::Result<reqwest::Client> {
    download_client_builder().build()
}

pub(crate) fn download_client_builder() -> reqwest::ClientBuilder {
    idle_timed_download_builder(DEFAULT_TIMEOUT)
}

fn idle_timed_download_builder(idle: Duration) -> reqwest::ClientBuilder {
    untimed_client_builder()
        .connect_timeout(idle)
        .read_timeout(idle)
        .redirect(reqwest::redirect::Policy::limited(DOWNLOAD_MAX_REDIRECTS))
}

pub fn build_version_download_client(version: HttpVersion) -> reqwest::Result<reqwest::Client> {
    let builder = download_client_builder();
    match version {
        HttpVersion::Http1 => builder
            .use_preconfigured_tls(rustls_config(&[b"http/1.1"]))
            .http1_only(),
        HttpVersion::Http2 => builder
            .use_preconfigured_tls(rustls_config(&[b"h2"]))
            .http2_prior_knowledge(),
    }
    .build()
}

pub fn download_client() -> &'static reqwest::Client {
    static DOWNLOAD_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    DOWNLOAD_CLIENT
        .get_or_init(|| build_download_client().expect("failed to build the download HTTP client"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_crypto_provider_is_idempotent_and_enables_tls() {
        init_crypto_provider();
        init_crypto_provider();

        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let _ = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
    }

    #[test]
    fn root_store_contains_webpki_baseline() {
        let roots = rustls_root_store();
        assert!(
            roots.len() >= webpki_roots::TLS_SERVER_ROOTS.len(),
            "root store missing webpki baseline",
        );
    }

    fn trickling_server(bytes: usize, gap: Duration) -> String {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let _ = stream.read(&mut [0u8; 4096]);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\n\r\n");
            for _ in 0..bytes {
                std::thread::sleep(gap);
                if stream.write_all(b"x").is_err() {
                    return;
                }
            }
        });

        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn download_outlasting_its_idle_timeout_completes_while_bytes_keep_arriving() {
        let url = trickling_server(5, Duration::from_millis(300));
        let client = idle_timed_download_builder(Duration::from_millis(800))
            .build()
            .expect("client");

        let body = client
            .get(url)
            .send()
            .await
            .expect("response")
            .bytes()
            .await
            .expect("body");

        assert_eq!(body.as_ref(), b"xxxxx");
    }

    #[tokio::test]
    async fn download_stalled_past_its_idle_timeout_fails() {
        let url = trickling_server(1, Duration::from_secs(3));
        let client = idle_timed_download_builder(Duration::from_millis(300))
            .build()
            .expect("client");

        let err = match client.get(url).send().await {
            Ok(response) => response.bytes().await.expect_err("stalled body"),
            Err(err) => err,
        };

        assert!(err.is_timeout(), "{err:?}");
    }
}
