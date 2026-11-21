/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::commit_file;
use super::fixtures::{
    build_git_comment_pkt_lines_with_min_byte_len,
    build_protocol_v0_ref_advertisement_pkt_lines_over_64_mib_listing_limit_with_flush_packet,
    build_v0_ref_advertisement_for_main_with_multi_ack,
    spawn_loopback_tcp_listener_counting_accepted_connections,
};
use crate::SourceError;
use crate::git::allowlisted_git_smart_http_subtransport::{
    GitHttpSmartSubtransport, find_https_credential_matching_url_host,
};
use crate::git::checkout::{Origin, fetch_commit_into_new_bare_temporary_repository};
use crate::git::git_credentials::{
    GitOperationCredentials, HttpsGitCredential,
    run_blocking_libgit2_operation_with_git_credentials,
};
use crate::git::git_network_limits::BlockingGitFetchOperationLimits;
use crate::git::git_transport_registration::register_git_transports_once;
use crate::git::guarded_git_network_connections::set_guarded_git_client_to_allow_loopback_for_tests;
use crate::git::remote::{branch_head, list_remote_refs};
use futures::future::BoxFuture;
use git2::transport::Service;
use gradient_types::RepositoryUrl;
use gradient_util::http::{HttpClientForUserSuppliedUrls, init_crypto_provider};
use gradient_util::http_validation::{
    UserUrlDestinationAllowlist, parse_and_check_allowed_outbound_url,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

enum TestGitHttpServerResponse {
    ServeRepository(PathBuf),
    Unauthorized,
    AnswersEveryRequestWithNotFound,
    RedirectTo(String),
    ServeUploadPackAdvertisement(Vec<u8>),
    ServeRefAdvertisementAndUploadPackResponse {
        advertisement: Vec<u8>,
        upload_pack_response: Vec<u8>,
    },
}

struct StartedGitHttpTestServer {
    port: u16,
    recorded_requests: Arc<Mutex<Vec<String>>>,
}

impl StartedGitHttpTestServer {
    fn recorded_http_request_heads(&self) -> Vec<String> {
        self.recorded_requests.lock().unwrap().clone()
    }
}

fn start_git_http_test_server_with_header_recording(
    response: TestGitHttpServerResponse,
) -> StartedGitHttpTestServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let recorded_requests = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&recorded_requests);
    std::thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            serve_one_http_request_and_record_head(connection, &response, &recorder);
        }
    });
    StartedGitHttpTestServer {
        port,
        recorded_requests,
    }
}

fn serve_one_http_request_and_record_head(
    mut connection: TcpStream,
    response: &TestGitHttpServerResponse,
    recorder: &Mutex<Vec<String>>,
) {
    let mut reader = BufReader::new(connection.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    let content_length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|length| length.trim().parse().ok())
        })
        .unwrap_or(0);
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    recorder.lock().unwrap().push(head.clone());

    let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
    let (status, extra_header, payload) = match response {
        TestGitHttpServerResponse::Unauthorized => (
            "401 Unauthorized",
            "WWW-Authenticate: Basic realm=\"git\"\r\n".to_owned(),
            Vec::new(),
        ),
        TestGitHttpServerResponse::AnswersEveryRequestWithNotFound => {
            ("404 Not Found", String::new(), Vec::new())
        }
        TestGitHttpServerResponse::RedirectTo(location) => (
            "302 Found",
            format!("Location: {location}{target}\r\n"),
            Vec::new(),
        ),
        TestGitHttpServerResponse::ServeUploadPackAdvertisement(advertisement) => (
            "200 OK",
            "Content-Type: application/x-git-upload-pack-advertisement\r\n".to_owned(),
            advertisement.clone(),
        ),
        TestGitHttpServerResponse::ServeRefAdvertisementAndUploadPackResponse {
            advertisement,
            upload_pack_response,
        } => match target.contains("/info/refs") {
            true => (
                "200 OK",
                "Content-Type: application/x-git-upload-pack-advertisement\r\n".to_owned(),
                advertisement.clone(),
            ),
            false => (
                "200 OK",
                "Content-Type: application/x-git-upload-pack-result\r\n".to_owned(),
                upload_pack_response.clone(),
            ),
        },
        TestGitHttpServerResponse::ServeRepository(repository) => {
            let (content_type, payload) = git_smart_http_response(repository, &target, &body);
            (
                "200 OK",
                format!("Content-Type: {content_type}\r\n"),
                payload,
            )
        }
    };
    let _ = write!(
        connection,
        "HTTP/1.1 {status}\r\n{extra_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = connection.write_all(&payload);
}

fn git_smart_http_response(repository: &Path, target: &str, body: &[u8]) -> (String, Vec<u8>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if let Some(service) = query.strip_prefix("service=") {
        let service_line = format!("# service={service}\n");
        let mut payload = format!("{:04x}{service_line}0000", service_line.len() + 4).into_bytes();
        payload.extend(run_git_stateless_rpc_on_repository(
            service,
            repository,
            &["--advertise-refs"],
            &[],
        ));
        return (format!("application/x-{service}-advertisement"), payload);
    }
    let service = path.rsplit('/').next().unwrap_or_default();
    (
        format!("application/x-{service}-result"),
        run_git_stateless_rpc_on_repository(service, repository, &[], body),
    )
}

fn run_git_stateless_rpc_on_repository(
    service: &str,
    repository: &Path,
    arguments: &[&str],
    input: &[u8],
) -> Vec<u8> {
    let program = service.strip_prefix("git-").unwrap();
    let mut child = Command::new("git")
        .arg(program)
        .arg("--stateless-rpc")
        .args(arguments)
        .arg(repository)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap().stdout
}

fn git_operation_credentials_with_admin_allowlist_from_entries(
    entries: &[&str],
) -> GitOperationCredentials {
    let entries: Vec<String> = entries.iter().map(|entry| (*entry).to_owned()).collect();
    let allowlist = UserUrlDestinationAllowlist::parse(&entries).unwrap();
    GitOperationCredentials::default()
        .with_guarded_http_client(HttpClientForUserSuppliedUrls::new(allowlist).unwrap())
}

fn resolve_every_host_to_loopback(
    _host: String,
    port: u16,
) -> BoxFuture<'static, std::io::Result<Vec<SocketAddr>>> {
    Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], port))]) })
}

fn ref_list_and_fetch_admin_allowlist_refusals(
    url: &RepositoryUrl,
    credentials: &GitOperationCredentials,
) -> [String; 2] {
    let listing_reason = match list_remote_refs(url, credentials).map(drop) {
        Err(SourceError::GitServerAddressRefusedByAdminAllowlist { reason }) => reason,
        result => panic!("Expected rejection from the admin allowlist, but got {result:?}."),
    };
    let fetched =
        fetch_commit_into_new_bare_temporary_repository(url, &"a".repeat(40), credentials)
            .map(drop);
    let fetch_reason = match fetched.as_ref().map_err(|error| error.downcast_ref()) {
        Err(Some(SourceError::GitServerAddressRefusedByAdminAllowlist { reason })) => {
            reason.clone()
        }
        _ => panic!("Expected rejection from the admin allowlist, but got {fetched:?}."),
    };
    [listing_reason, fetch_reason]
}

fn create_bare_git_repository_with_head_at_main(directory: &Path) -> git2::Repository {
    let repository = git2::Repository::init_bare(directory).unwrap();
    repository.set_head("refs/heads/main").unwrap();
    repository
}

fn stored_https_git_credential_with_fixed_password_token(
    host: &str,
    username: &str,
) -> HttpsGitCredential {
    HttpsGitCredential {
        host: host.to_owned(),
        username: username.to_owned(),
        password: "token".into(),
    }
}

fn get_authorization_header_value_from_raw_request_head(head: &str) -> Option<&str> {
    head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then_some(value.trim())
    })
}

#[test]
fn test_public_only_allowlist_refuses_ref_listing_and_fetch_for_loopback_and_internal_host_and_accepts_no_connection()
 {
    let (port, accepted) = spawn_loopback_tcp_listener_counting_accepted_connections();
    let loopback_url: RepositoryUrl = format!("http://127.0.0.1:{port}/r.git").parse().unwrap();
    let public_only = UserUrlDestinationAllowlist::default();
    let literal_reason =
        parse_and_check_allowed_outbound_url(loopback_url.git_connection_url(), &public_only)
            .unwrap_err()
            .to_string();

    let loopback_reasons = ref_list_and_fetch_admin_allowlist_refusals(
        &loopback_url,
        &GitOperationCredentials::default()
            .with_guarded_http_client(HttpClientForUserSuppliedUrls::new(public_only).unwrap()),
    );

    assert_eq!(loopback_reasons, [literal_reason.clone(), literal_reason]);
    let internal_url: RepositoryUrl = format!("http://internal.example:{port}/r.git")
        .parse()
        .unwrap();
    let faked_dns = UserUrlDestinationAllowlist::default()
        .with_dns_lookup_function(resolve_every_host_to_loopback);
    let internal_reasons = ref_list_and_fetch_admin_allowlist_refusals(
        &internal_url,
        &GitOperationCredentials::default()
            .with_guarded_http_client(HttpClientForUserSuppliedUrls::new(faked_dns).unwrap()),
    );
    for reason in internal_reasons {
        assert!(reason.contains("internal.example"), "{reason}");
        assert!(!reason.contains("127.0.0.1"), "{reason}");
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "The listener accepted the connection, but this address is not on the admin allowlist."
    );
}

#[test]
fn test_redirect_to_address_not_in_allowlist_fails_listing_without_connection() {
    let not_allowed = TcpListener::bind("127.0.0.2:0").unwrap();
    not_allowed.set_nonblocking(true).unwrap();
    let not_allowed_port = not_allowed.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        loop {
            if not_allowed.accept().is_ok() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    let redirecting = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::RedirectTo(format!("http://127.0.0.2:{not_allowed_port}")),
    );
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", redirecting.port)
        .parse()
        .unwrap();

    let reasons = ref_list_and_fetch_admin_allowlist_refusals(
        &url,
        &git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.1/32"]),
    );

    for reason in reasons {
        assert!(reason.contains("127.0.0.2"), "{reason}");
    }
    assert!(!redirecting.recorded_http_request_heads().is_empty());
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "The listener accepted the connection, but this address is not on the admin allowlist."
    );
}

#[test]
fn test_plain_http_ref_listing_full_and_shallow_fetch_with_loopback_allowed_do_not_send_stored_https_credential()
 {
    let directory = tempfile::TempDir::new().unwrap();
    let repository = create_bare_git_repository_with_head_at_main(directory.path());
    let first = commit_file(&repository, "refs/heads/main", "first");
    let tip = commit_file(&repository, "refs/heads/main", "second");
    let server = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::ServeRepository(directory.path().to_owned()),
    );
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", server.port)
        .parse()
        .unwrap();
    let credentials = GitOperationCredentials::from_ssh_key_known_hosts_and_https_credentials(
        None,
        vec![HttpsGitCredential {
            host: format!("127.0.0.1:{}", server.port),
            username: "x-access-token".to_owned(),
            password: "token".into(),
        }],
    )
    .with_guarded_http_client(
        git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"])
            .http_client()
            .clone(),
    );

    let refs = list_remote_refs(&url, &credentials).unwrap();
    let shallow =
        fetch_commit_into_new_bare_temporary_repository(&url, &tip.to_string(), &credentials)
            .unwrap();
    let full_directory = tempfile::TempDir::new().unwrap();
    let full = git2::Repository::init_bare(full_directory.path()).unwrap();
    run_blocking_libgit2_operation_with_git_credentials(
        &credentials,
        BlockingGitFetchOperationLimits::ordinary_fetch_operation_limits(),
        || -> anyhow::Result<()> {
            Origin::new(&full, url.git_connection_url())?
                .fetch("+refs/heads/*:refs/remotes/origin/*", false)?;
            Ok(())
        },
    )
    .unwrap();

    let head = branch_head(&refs, None).unwrap();
    assert_eq!(head.branch, "refs/heads/main");
    assert_eq!(head.commit, tip.as_bytes());
    assert!(shallow.repository().is_shallow());
    assert!(!full.is_shallow());
    full.find_commit(first).unwrap();
    for head in server.recorded_http_request_heads() {
        assert!(
            get_authorization_header_value_from_raw_request_head(&head).is_none(),
            "The request sent stored HTTPS credentials over plaintext HTTP: {head}"
        );
    }
}

#[test]
fn an_info_refs_listing_larger_than_the_64_mib_limit_fails_with_the_refusal_text() {
    let service_line = "# service=git-upload-pack\n";
    let mut advertisement =
        format!("{:04x}{service_line}0000", service_line.len() + 4).into_bytes();
    advertisement.extend(
        build_protocol_v0_ref_advertisement_pkt_lines_over_64_mib_listing_limit_with_flush_packet(),
    );
    let server = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::ServeUploadPackAdvertisement(advertisement),
    );
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", server.port)
        .parse()
        .unwrap();
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);

    let listed = list_remote_refs(&url, &credentials).map(drop);

    match listed {
        Err(SourceError::GitCommandFailed { stderr }) => {
            assert!(
                stderr.contains("ref advertisement larger than 64 MiB"),
                "{stderr}"
            );
        }
        result => panic!(
            "Expected the ref listing to stop at the byte limit of the ref advertisement, but got {result:?}."
        ),
    }
}

#[test]
fn smart_http_listing_redirect_to_another_allowed_host_is_refused_with_readable_reason_and_no_second_request()
 {
    let (other_host_port, accepted) = spawn_loopback_tcp_listener_counting_accepted_connections();
    let redirecting = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::RedirectTo(format!("http://localhost:{other_host_port}")),
    );
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", redirecting.port)
        .parse()
        .unwrap();
    let credentials =
        git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8", "localhost"]);

    let reason = list_remote_refs(&url, &credentials)
        .map(drop)
        .map_err(|error| error.short_reason_for_user());

    let redirect_reason = "The server redirected to a different host.";
    assert!(
        matches!(&reason, Err(text) if text.starts_with(redirect_reason)),
        "{reason:?}"
    );
    assert!(!redirecting.recorded_http_request_heads().is_empty());
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "The second server accepted a connection, but the required behavior for Gradient is rejection of a redirect to another host before connecting."
    );
}

#[test]
fn git_smart_http_list_and_fetch_same_host_redirect_succeeds() {
    let directory = tempfile::TempDir::new().unwrap();
    let repository = create_bare_git_repository_with_head_at_main(directory.path());
    let tip = commit_file(&repository, "refs/heads/main", "tip");
    let serving = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::ServeRepository(directory.path().to_owned()),
    );
    let redirecting = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::RedirectTo(format!("http://127.0.0.1:{}", serving.port)),
    );
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", redirecting.port)
        .parse()
        .unwrap();
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);

    let refs = list_remote_refs(&url, &credentials).unwrap();
    fetch_commit_into_new_bare_temporary_repository(&url, &tip.to_string(), &credentials).unwrap();

    assert_eq!(branch_head(&refs, None).unwrap().commit, tip.as_bytes());
    assert!(!redirecting.recorded_http_request_heads().is_empty());
}

#[test]
fn smart_http_ref_listing_404_shows_no_repository_at_this_url() {
    let server = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::AnswersEveryRequestWithNotFound,
    );
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", server.port)
        .parse()
        .unwrap();
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);

    let reason = list_remote_refs(&url, &credentials)
        .map(drop)
        .map_err(|error| error.short_reason_for_user());

    assert_eq!(
        reason,
        Err("The server has no repository at this URL.".to_owned())
    );
}

#[test]
fn upload_pack_post_fetch_stops_at_byte_limit_when_server_sends_megabytes_of_comment_packets() {
    let service_line = "# service=git-upload-pack\n";
    let mut advertisement =
        format!("{:04x}{service_line}0000", service_line.len() + 4).into_bytes();
    advertisement.extend(build_v0_ref_advertisement_for_main_with_multi_ack(
        &"1".repeat(40),
    ));
    let server = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::ServeRefAdvertisementAndUploadPackResponse {
            advertisement,
            upload_pack_response: build_git_comment_pkt_lines_with_min_byte_len(8 << 20),
        },
    );
    let url = format!("http://127.0.0.1:{}/r.git", server.port);
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);
    let target = tempfile::TempDir::new().unwrap();
    let limits = BlockingGitFetchOperationLimits {
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
        byte_limit: 1 << 20,
        finished_fetch_bytes: 0,
    };

    let fetched = run_blocking_libgit2_operation_with_git_credentials(
        &credentials,
        limits,
        || -> anyhow::Result<()> {
            let target = git2::Repository::init_bare(target.path())?;
            Origin::new(&target, &url)?.fetch("refs/heads/main", false)?;
            Ok(())
        },
    );

    let refusal = "The received byte count went past the fetch limit.";
    let error = fetched.as_ref().map_err(|error| format!("{error:#}"));
    assert!(
        matches!(&error, Err(text) if text.contains(refusal)),
        "Expected the fetch to stop at the limit on bytes per fetch, but got {fetched:?}."
    );
}

const UNKNOWN_AUTHORITY_TEST_CERTIFICATE_FOR_127_0_0_1_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBmTCCAUCgAwIBAgIUB16mZdaFj5WvN4VMhNQmIOtTvYIwCgYIKoZIzj0EAwIw
ITEfMB0GA1UEAwwWdW5rbm93bi10ZXN0LWF1dGhvcml0eTAgFw0yNjExMjExNDAz
MTZaGA8yMTI2MTAyODE0MDMxNlowFDESMBAGA1UEAwwJMTI3LjAuMC4xMFkwEwYH
KoZIzj0CAQYIKoZIzj0DAQcDQgAEbasECHKCYaqMR1IRX+hZg+ntK30E1yf2O8Oh
VhVLWxz7/HUM9YvZf/yJJMENOmMI+/lmfUnuVuh9rufdSdC6XqNhMF8wDwYDVR0R
BAgwBocEfwAAATAMBgNVHRMBAf8EAjAAMB0GA1UdDgQWBBQqV6b0B/Sq5J/vnQNs
RhhBGb16ETAfBgNVHSMEGDAWgBQClNS2WyMXrPFmkPejSUT02levUDAKBggqhkjO
PQQDAgNHADBEAiBajLm0YIkhvmlDrykmyAHfLdhxV4aMZ8rALdJFlfNx2QIgf9Xf
sYSSGggNS5HB7irM2uq1vg3aFXoE3uZ7E+Glpk0=
-----END CERTIFICATE-----
";

const UNKNOWN_AUTHORITY_TEST_CERTIFICATE_PKCS8_PRIVATE_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgY7fLgxuIPitDYU5j
KsKgBG8iI3AuqSql1OV2otYLRNOhRANCAARtqwQIcoJhqoxHUhFf6FmD6e0rfQTX
J/Y7w6FWFUtbHPv8dQz1i9l//IkkwQ06Ywj7+WZ9Se5W6H2u591J0Lpe
-----END PRIVATE KEY-----
";

fn start_loopback_tls_listener_with_unknown_test_authority_certificate_and_return_port() -> u16 {
    init_crypto_provider();
    let certificate = CertificateDer::from_pem_slice(
        UNKNOWN_AUTHORITY_TEST_CERTIFICATE_FOR_127_0_0_1_PEM.as_bytes(),
    )
    .unwrap();
    let key = PrivateKeyDer::from_pem_slice(
        UNKNOWN_AUTHORITY_TEST_CERTIFICATE_PKCS8_PRIVATE_KEY_PEM.as_bytes(),
    )
    .unwrap();
    let config = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for connection in listener.incoming().flatten() {
            let tls = rustls::ServerConnection::new(Arc::clone(&config)).unwrap();
            let _ = rustls::StreamOwned::new(tls, connection).read(&mut [0; 1]);
        }
    });
    port
}

#[test]
fn ref_listing_over_https_with_untrusted_certificate_reports_invalid_peer_certificate() {
    let port =
        start_loopback_tls_listener_with_unknown_test_authority_certificate_and_return_port();
    let url: RepositoryUrl = format!("https://127.0.0.1:{port}/r.git").parse().unwrap();
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);

    let listed = list_remote_refs(&url, &credentials).map(drop);

    match listed {
        Err(SourceError::GitCommandFailed { stderr }) => {
            assert!(
                stderr.contains("invalid peer certificate: UnknownIssuer"),
                "{stderr}"
            );
        }
        result => panic!(
            "Expected the ref listing to fail for an untrusted server certificate, but the call returned {result:?}."
        ),
    }
}

#[test]
fn test_listing_fails_with_unchanged_authentication_errors_on_401() {
    let server =
        start_git_http_test_server_with_header_recording(TestGitHttpServerResponse::Unauthorized);
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);
    let url: RepositoryUrl = format!("http://127.0.0.1:{}/r.git", server.port)
        .parse()
        .unwrap();

    let without_credentials = list_remote_refs(&url, &credentials).map(drop);
    let with_user_info = run_blocking_libgit2_operation_with_git_credentials(
        &credentials,
        BlockingGitFetchOperationLimits::ordinary_fetch_operation_limits(),
        || -> anyhow::Result<()> {
            git2::Remote::create_detached(format!(
                "http://ci:token@127.0.0.1:{}/r.git",
                server.port
            ))?
            .connect(git2::Direction::Fetch)?;
            Ok(())
        },
    );

    match without_credentials {
        Err(SourceError::GitCommandFailed { stderr }) => assert!(
            stderr.contains("remote authentication required but no callback set"),
            "{stderr}"
        ),
        result => panic!("{result:?}"),
    }
    let rejected = format!("{:#}", with_user_info.unwrap_err());
    assert!(
        rejected.contains(&format!(
            "authentication failed: the server for 127.0.0.1:{} rejected the HTTPS credentials",
            server.port
        )),
        "{rejected}"
    );
}

#[test]
fn https_credential_lookup_matches_only_exact_host_and_port() {
    let stored = [
        stored_https_git_credential_with_fixed_password_token("github.com", "ci"),
        stored_https_git_credential_with_fixed_password_token("git.example:8443", "ci"),
    ];
    let host_for = |url: &str| {
        find_https_credential_matching_url_host(&stored, url)
            .map(|credential| credential.host.as_str())
    };

    assert_eq!(host_for("https://github.com/o/r.git"), Some("github.com"));
    assert_eq!(host_for("https://GitHub.COM/o/r.git"), Some("github.com"));
    assert_eq!(
        host_for("git+https://user@git.example:8443/o/r?dir=nix"),
        Some("git.example:8443")
    );
    for url in [
        "http://github.com/o/r.git",
        "https://api.github.com/o/r.git",
        "https://github.com.evil.example/o/r.git",
        "https://github.com:8443/o/r.git",
        "https://git.example/o/r.git",
        "ssh://git@github.com/o/r.git",
    ] {
        assert_eq!(host_for(url), None, "{url}");
    }
}

#[test]
fn test_https_request_has_basic_authorization_header_from_stored_credential() {
    let subtransport = GitHttpSmartSubtransport::new(
        HttpClientForUserSuppliedUrls::new(UserUrlDestinationAllowlist::default()).unwrap(),
        vec![
            stored_https_git_credential_with_fixed_password_token("github.com", "x-access-token"),
            stored_https_git_credential_with_fixed_password_token("gitea.example:3000", "gradient"),
        ],
    );
    let authorization_header_value = |url: &str, service: Service| {
        let request = subtransport
            .request_stream_for_service(url, service)
            .unwrap()
            .request_builder()
            .unwrap()
            .build()
            .unwrap();
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .map(|value| value.to_str().unwrap().to_owned())
    };

    for (url, expected) in [
        (
            "https://github.com/o/r.git",
            "Basic eC1hY2Nlc3MtdG9rZW46dG9rZW4=",
        ),
        (
            "https://gitea.example:3000/o/r.git",
            "Basic Z3JhZGllbnQ6dG9rZW4=",
        ),
    ] {
        for service in [Service::UploadPackLs, Service::UploadPack] {
            assert_eq!(
                authorization_header_value(url, service).as_deref(),
                Some(expected)
            );
        }
    }
    assert_eq!(
        authorization_header_value("https://evil.example/o/r.git", Service::UploadPackLs),
        None
    );
}

#[test]
fn ref_listing_and_fetch_from_local_git_smart_http_server_with_token_in_url_user_info_as_github_app_and_gitea_forms()
 {
    let directory = tempfile::TempDir::new().unwrap();
    let repository = create_bare_git_repository_with_head_at_main(directory.path());
    let tip = commit_file(&repository, "refs/heads/main", "tip");
    let credentials = git_operation_credentials_with_admin_allowlist_from_entries(&["127.0.0.0/8"]);

    for (user_info, expected) in [
        ("x-access-token:tok", "Basic eC1hY2Nlc3MtdG9rZW46dG9r"),
        ("tok", "Basic dG9rOg=="),
    ] {
        let server = start_git_http_test_server_with_header_recording(
            TestGitHttpServerResponse::ServeRepository(directory.path().to_owned()),
        );
        let url: RepositoryUrl = format!("http://{user_info}@127.0.0.1:{}/r.git", server.port)
            .parse()
            .unwrap();

        list_remote_refs(&url, &credentials).unwrap();
        fetch_commit_into_new_bare_temporary_repository(&url, &tip.to_string(), &credentials)
            .unwrap();

        let heads = server.recorded_http_request_heads();
        assert!(
            heads
                .iter()
                .any(|head| head.starts_with("POST /r.git/git-upload-pack "))
        );
        for head in heads {
            assert_eq!(
                get_authorization_header_value_from_raw_request_head(&head),
                Some(expected),
                "The request didn't include a header for Basic Authorization with the token from the user information in the URL."
            );
        }
    }
}

#[test]
fn test_write_to_request_stream_over_body_limit_fails_and_keeps_body_unchanged() {
    let subtransport = GitHttpSmartSubtransport::new(
        HttpClientForUserSuppliedUrls::new(UserUrlDestinationAllowlist::default()).unwrap(),
        Vec::new(),
    );
    let mut stream = subtransport
        .request_stream_for_service("https://git.example/o/r.git", Service::ReceivePack)
        .unwrap();
    stream.request_body_byte_limit = 4;

    stream.write_all(b"abc").unwrap();
    let over_limit = stream.write(b"de").unwrap_err();
    stream.write_all(b"d").unwrap();

    assert!(
        over_limit.to_string().contains("larger than 4 bytes"),
        "{over_limit}"
    );
}

#[test]
fn test_push_with_user_and_password_in_url_updates_branch() {
    register_git_transports_once();
    set_guarded_git_client_to_allow_loopback_for_tests();
    let server_directory = tempfile::TempDir::new().unwrap();
    let server_repository = create_bare_git_repository_with_head_at_main(server_directory.path());
    let base = commit_file(&server_repository, "refs/heads/main", "base");
    let server = start_git_http_test_server_with_header_recording(
        TestGitHttpServerResponse::ServeRepository(server_directory.path().to_owned()),
    );
    let url = format!(
        "http://x-access-token:token@127.0.0.1:{}/r.git",
        server.port
    );
    let local_directory = tempfile::TempDir::new().unwrap();
    let local = git2::Repository::init_bare(local_directory.path()).unwrap();
    let mut remote = local.remote_anonymous(&url).unwrap();
    let mut fetch = git2::FetchOptions::new();
    fetch.depth(1);

    remote
        .fetch(&["refs/heads/main"], Some(&mut fetch), None)
        .unwrap();
    let pushed = commit_file(&local, "refs/heads/update", "update");
    remote
        .push(
            &["+refs/heads/update:refs/heads/update"],
            Some(&mut git2::PushOptions::new()),
        )
        .unwrap();

    assert_eq!(
        local.find_reference("FETCH_HEAD").unwrap().target(),
        Some(base)
    );
    assert_eq!(
        server_repository
            .find_reference("refs/heads/update")
            .unwrap()
            .target(),
        Some(pushed)
    );
    let heads = server.recorded_http_request_heads();
    let push_head = heads
        .iter()
        .find(|head| head.starts_with("POST /r.git/git-receive-pack "))
        .unwrap();
    assert_eq!(
        get_authorization_header_value_from_raw_request_head(push_head),
        Some("Basic eC1hY2Nlc3MtdG9rZW46dG9rZW4="),
        "Push requests to git receive-pack carry no header for Basic Authorization with the username and password from the URL."
    );
    assert!(heads.iter().all(|head| !head.contains("token@")));
}
