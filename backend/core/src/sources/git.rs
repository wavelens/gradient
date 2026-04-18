/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{FlakePrefetcher, PrefetchedFlake, SourceError};
use crate::types::input::{check_repository_url_is_ssh, vec_to_hex};
use crate::types::*;
use anyhow::Result;
use async_trait::async_trait;
use entity::evaluation::EvaluationStatus;
use git2::{Direction, RemoteCallbacks};
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::{debug, info, instrument, warn};

// ── ProjectGitContext ─────────────────────────────────────────────────────────

/// Bundles the server state, project reference, and (if the repository URL is
/// SSH) the decrypted key pair for the project's owning organisation.
///
/// Created once per project-check cycle via [`ProjectGitContext::new`]. Both
/// [`check_project_updates`] and [`get_commit_info`] are thin wrappers that
/// construct this context and call the corresponding method, so the DB round-
/// trip and key decryption only happen once even when both are called in
/// sequence (e.g. in `dispatch::poll_projects_for_evaluations`).
struct ProjectGitContext<'a> {
    state: &'a Arc<ServerState>,
    project: &'a MProject,
    /// `Some((private_key, public_key))` for SSH repos; `None` for HTTPS/git.
    ssh_creds: Option<(String, String)>,
}

impl<'a> ProjectGitContext<'a> {
    /// Resolve SSH credentials from the DB if the repository URL is SSH.
    async fn new(state: &'a Arc<ServerState>, project: &'a MProject) -> Result<Self, SourceError> {
        let url = &project.repository;
        let ssh_creds = if check_repository_url_is_ssh(url) {
            let organization = EOrganization::find_by_id(project.organization)
                .one(&state.db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?
                .ok_or(SourceError::OrganizationNotFound {
                    id: project.organization,
                })?;
            Some(super::ssh_key::decrypt_ssh_private_key(
                state.cli.crypt_secret_file.clone(),
                organization,
                &state.cli.serve_url,
            )?)
        } else {
            None
        };
        Ok(Self {
            state,
            project,
            ssh_creds,
        })
    }

    /// Check whether there is a new commit on the remote HEAD.
    ///
    /// Returns `(has_update, remote_hash)`. `has_update` is `false` when the
    /// remote HEAD matches the last evaluated commit or an evaluation is already
    /// in progress.
    #[instrument(skip(self), fields(project_id = %self.project.id, project_name = %self.project.name))]
    async fn check_for_updates(&self) -> Result<(bool, Vec<u8>), SourceError> {
        debug!("Checking for updates on project");

        let url = self.project.repository.clone();
        let ssh_creds = self.ssh_creds.clone();

        let remote_hash = match tokio::task::spawn_blocking(move || {
            if let Some((private_key, public_key)) = ssh_creds {
                ls_remote_head(&url, Some(&private_key), Some(&public_key))
            } else {
                ls_remote_head(&url, None, None)
            }
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        })? {
            Ok(hash) => hash,
            Err(e) => {
                warn!(error = %e, "Failed to get remote HEAD ref, will retry next cycle");
                return Ok((false, vec![]));
            }
        };

        let remote_hash_str = vec_to_hex(&remote_hash);
        debug!(remote_hash = %remote_hash_str, "Retrieved remote hash");

        if self.project.force_evaluation {
            info!("Force evaluation enabled, updating project");
            return Ok((true, remote_hash));
        }

        if let Some(last_evaluation) = self.project.last_evaluation {
            let evaluation = EEvaluation::find_by_id(last_evaluation)
                .one(&self.state.db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?
                .ok_or_else(|| SourceError::Database {
                    reason: "Evaluation not found".to_string(),
                })?;

            if evaluation.status == EvaluationStatus::Queued
                || evaluation.status == EvaluationStatus::Fetching
                || evaluation.status == EvaluationStatus::EvaluatingFlake
                || evaluation.status == EvaluationStatus::EvaluatingDerivation
                || evaluation.status == EvaluationStatus::Building
                || evaluation.status == EvaluationStatus::Waiting
            {
                debug!(status = ?evaluation.status, "Evaluation already in progress, skipping");
                return Ok((false, remote_hash));
            }

            let commit = ECommit::find_by_id(evaluation.commit)
                .one(&self.state.db)
                .await
                .map_err(|e| SourceError::Database {
                    reason: e.to_string(),
                })?
                .ok_or_else(|| SourceError::Database {
                    reason: "Commit not found".to_string(),
                })?;

            if commit.hash == remote_hash {
                debug!("Remote hash matches current evaluation commit, no update needed");
                return Ok((false, remote_hash));
            }

            info!("Remote hash differs from current evaluation commit, update needed");
        } else {
            info!("No previous evaluation found, update needed");
        }

        Ok((true, remote_hash))
    }

    /// Clone the repository at `commit_hash` and extract the commit metadata.
    ///
    /// Returns `(message, author_email, author_name)`.
    #[instrument(skip(self), fields(project_id = %self.project.id, project_name = %self.project.name, commit_hash = %vec_to_hex(commit_hash)))]
    async fn commit_info(
        &self,
        commit_hash: &[u8],
    ) -> Result<(String, Option<String>, String), SourceError> {
        debug!("Fetching commit info");

        let hash_str = vec_to_hex(commit_hash);
        let url = self.project.repository.clone();
        let ssh_creds = self.ssh_creds.clone();

        let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
            reason: e.to_string(),
        })?;

        let temp_path = temp_dir.path().to_path_buf();

        tokio::task::spawn_blocking(move || {
            let mut callbacks = RemoteCallbacks::new();
            callbacks
                .certificate_check(|_cert, _valid| Ok(git2::CertificateCheckStatus::CertificateOk));

            if let Some((private_key, public_key)) = ssh_creds {
                callbacks.credentials(move |_url, username_from_url, _allowed| {
                    git2::Cred::ssh_key_from_memory(
                        username_from_url.unwrap_or("git"),
                        Some(&public_key),
                        &private_key,
                        None,
                    )
                });
            }

            let mut fo = git2::FetchOptions::new();
            fo.remote_callbacks(callbacks);

            let mut builder = git2::build::RepoBuilder::new();
            builder.bare(true);
            builder.fetch_options(fo);
            let repo =
                builder
                    .clone(&url, &temp_path)
                    .map_err(|e| SourceError::GitCommandFailed {
                        stderr: e.message().to_string(),
                    })?;

            let oid = git2::Oid::from_str(&hash_str).map_err(|_| SourceError::GitOutputParsing)?;
            let commit = repo
                .find_commit(oid)
                .map_err(|e| SourceError::GitCommandFailed {
                    stderr: e.message().to_string(),
                })?;

            let message = commit.summary().unwrap_or("").to_string();
            let author_email = commit.author().email().map(|s| s.to_string());
            let author_name = commit.author().name().unwrap_or("").to_string();

            Ok((message, author_email, author_name))
        })
        .await
        .map_err(|e| SourceError::GitExecution {
            error: e.to_string(),
        })?
    }
}

// ── Public entry points ───────────────────────────────────────────────────────

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name))]
pub async fn check_project_updates(
    state: Arc<ServerState>,
    project: &MProject,
) -> Result<(bool, Vec<u8>), SourceError> {
    ProjectGitContext::new(&state, project)
        .await?
        .check_for_updates()
        .await
}

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name, commit_hash = %vec_to_hex(commit_hash)))]
pub async fn get_commit_info(
    state: Arc<ServerState>,
    project: &MProject,
    commit_hash: &[u8],
) -> Result<(String, Option<String>, String), SourceError> {
    ProjectGitContext::new(&state, project)
        .await?
        .commit_info(commit_hash)
        .await
}

// ── Libgit2Prefetcher ─────────────────────────────────────────────────────────

/// Production `FlakePrefetcher` backed by libgit2 + the Nix C API.
#[derive(Debug, Default)]
pub struct Libgit2Prefetcher;

impl Libgit2Prefetcher {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FlakePrefetcher for Libgit2Prefetcher {
    async fn prefetch(
        &self,
        crypt_secret_file: String,
        serve_url: String,
        repository: String,
        organization: MOrganization,
    ) -> Result<Option<PrefetchedFlake>> {
        prefetch_flake_inner(crypt_secret_file, serve_url, repository, organization)
            .await
            .map(|opt| opt.map(PrefetchedFlake::from_tempdir))
            .map_err(|e| anyhow::anyhow!("{}", e))
    }
}

#[instrument(skip(organization), fields(repository = %repository))]
async fn prefetch_flake_inner(
    crypt_secret_file: String,
    serve_url: String,
    repository: String,
    organization: MOrganization,
) -> std::result::Result<Option<tempfile::TempDir>, SourceError> {
    if !check_repository_url_is_ssh(&repository) {
        debug!("HTTPS repository – skipping git clone, nix will fetch on demand");
        return Ok(None);
    }

    debug!("SSH repository – cloning via libgit2: {}", repository);

    let (private_key, public_key) =
        super::ssh_key::decrypt_ssh_private_key(crypt_secret_file, organization, &serve_url)?;

    let (git_url, rev) = parse_nix_git_url(&repository)?;

    let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;

    let temp_path = temp_dir.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let fo = make_ssh_fetch_options(&private_key, &public_key);

        let repo = git2::build::RepoBuilder::new()
            .fetch_options(fo)
            .clone(&git_url, &temp_path)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let oid = git2::Oid::from_str(&rev).map_err(|_| SourceError::GitOutputParsing)?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let tree = commit.tree().map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        repo.checkout_tree(tree.as_object(), Some(&mut co))
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        repo.set_head_detached(oid)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        debug!("Cloned repository to {:?} at rev {}", temp_path, rev);

        crate::nix::lock_flake_with_ssh_key(&temp_path, &private_key).map_err(|e| {
            SourceError::NixFlakeArchiveFailed {
                stderr: e.to_string(),
            }
        })?;

        debug!("Locked flake and prefetched inputs for {:?}", temp_path);

        Ok::<(), SourceError>(())
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    })??;

    Ok(Some(temp_dir))
}

// ── Low-level git helpers ─────────────────────────────────────────────────────

/// Build `FetchOptions` with in-memory SSH credentials.
/// The key strings are cloned so the closure is `'static`.
fn make_ssh_fetch_options(private_key: &str, public_key: &str) -> git2::FetchOptions<'static> {
    let priv_key = private_key.to_owned();
    let pub_key = public_key.to_owned();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|_cert, _valid| Ok(git2::CertificateCheckStatus::CertificateOk));
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        git2::Cred::ssh_key_from_memory(
            username_from_url.unwrap_or("git"),
            Some(&pub_key),
            &priv_key,
            None,
        )
    });

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);
    fo
}

fn ls_remote_head(
    url: &str,
    private_key: Option<&str>,
    public_key: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    match (private_key, public_key) {
        (Some(priv_key), Some(pub_key)) => ls_remote_head_ssh(url, priv_key, pub_key),
        _ if url.starts_with("git://") => ls_remote_head_git_protocol(url),
        _ => ls_remote_head_no_creds(url),
    }
}

/// List the remote HEAD ref using the raw git wire protocol (v0) over TCP.
///
/// libgit2's `connect_auth` + `list()` can return an empty ref list for
/// `git://` URLs because it negotiates git protocol v2 with git-daemon, and
/// the subsequent `ls-refs` exchange may fail silently on some daemon versions.
/// This implementation sends a plain protocol-v0 pkt-line request (no
/// `version=2` extra parameter) so the daemon responds with an immediate v0
/// ref advertisement containing HEAD.
fn ls_remote_head_git_protocol(url: &str) -> Result<Vec<u8>, SourceError> {
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    // Parse git://[host[:port]]/path
    let rest = url.strip_prefix("git://").ok_or(SourceError::InvalidUrl)?;
    let (host_port, repo_path) = rest.split_once('/').ok_or(SourceError::InvalidUrl)?;
    let (host, port) = if let Some((h, p)) = host_port.rsplit_once(':') {
        (h, p.parse::<u16>().unwrap_or(9418))
    } else {
        (host_port, 9418u16)
    };

    let mut stream =
        TcpStream::connect((host, port)).map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    // Protocol-v0 request: "git-upload-pack /path\0host=host\0"
    // Deliberately omitting "version=2" so the daemon responds in v0 format.
    let body = format!("git-upload-pack /{}\0host={}\0", repo_path, host);
    let pkt = format!("{:04x}{}", body.len() + 4, body);
    stream
        .write_all(pkt.as_bytes())
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.to_string(),
        })?;

    read_head_from_pktlines(&mut stream)
}

/// List the remote HEAD ref via libgit2 with in-memory SSH credentials.
///
/// Used exclusively for SSH URLs where the private key must be supplied
/// in-memory without writing it to disk.
fn ls_remote_head_ssh(
    url: &str,
    private_key: &str,
    public_key: &str,
) -> Result<Vec<u8>, SourceError> {
    let mut remote =
        git2::Remote::create_detached(url).map_err(|e| SourceError::GitCommand(e.to_string()))?;

    let priv_key = private_key.to_string();
    let pub_key = public_key.to_string();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|_cert, _valid| Ok(git2::CertificateCheckStatus::CertificateOk));
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        git2::Cred::ssh_key_from_memory(
            username_from_url.unwrap_or("git"),
            Some(&pub_key),
            &priv_key,
            None,
        )
    });

    let conn = remote
        .connect_auth(Direction::Fetch, Some(callbacks), None)
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

    let list = conn.list().map_err(|e| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    })?;

    list.iter()
        .find(|h| h.name() == "HEAD")
        .or_else(|| list.first())
        .map(|h| h.oid().as_bytes().to_vec())
        .ok_or(SourceError::GitHashExtraction)
}

/// List the remote HEAD ref via libgit2 with no credentials (for https://).
fn ls_remote_head_no_creds(url: &str) -> Result<Vec<u8>, SourceError> {
    let mut remote =
        git2::Remote::create_detached(url).map_err(|e| SourceError::GitCommand(e.to_string()))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|_cert, _valid| Ok(git2::CertificateCheckStatus::CertificateOk));

    let conn = remote
        .connect_auth(Direction::Fetch, Some(callbacks), None)
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

    let list = conn.list().map_err(|e| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    })?;

    list.iter()
        .find(|h| h.name() == "HEAD")
        .or_else(|| list.first())
        .map(|h| h.oid().as_bytes().to_vec())
        .ok_or(SourceError::GitHashExtraction)
}

/// Read pkt-lines from `reader` and return the SHA-1 hash of HEAD.
///
/// Reads incrementally — one pkt-line at a time — so it works correctly
/// even when the remote keeps the connection open after the ref advertisement
/// (which is normal git protocol behavior).
///
/// Falls back to the first non-zero ref if HEAD is not listed (e.g. empty repo
/// advertising only `capabilities^{}`).
fn read_head_from_pktlines(reader: &mut dyn std::io::Read) -> Result<Vec<u8>, SourceError> {
    let mut len_buf = [0u8; 4];
    let mut first_ref: Option<Vec<u8>> = None;

    loop {
        std::io::Read::read_exact(reader, &mut len_buf).map_err(|e| {
            SourceError::GitCommandFailed {
                stderr: e.to_string(),
            }
        })?;

        let len = std::str::from_utf8(&len_buf)
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or(SourceError::GitOutputParsing)?;

        if len == 0 {
            break; // flush pkt — end of advertisement
        }

        if len < 4 {
            break;
        }

        let payload_len = len - 4;
        let mut data = vec![0u8; payload_len];
        std::io::Read::read_exact(reader, &mut data).map_err(|e| {
            SourceError::GitCommandFailed {
                stderr: e.to_string(),
            }
        })?;

        // Ref lines: "<40-hex-sha1> <refname>[NUL capabilities]\n"
        if data.len() >= 41 && data[40] == b' ' {
            let sha = match std::str::from_utf8(&data[..40]) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let ref_bytes = &data[41..];
            let refname_end = ref_bytes
                .iter()
                .position(|&b| b == 0 || b == b'\n')
                .unwrap_or(ref_bytes.len());

            let refname = std::str::from_utf8(&ref_bytes[..refname_end])
                .unwrap_or("")
                .trim();

            debug!(refname, sha, "pkt-line ref");

            if refname == "HEAD" {
                return hex::decode(sha).map_err(|_| SourceError::GitOutputParsing);
            }

            // Remember the first real ref as fallback (skip zero-id capabilities marker).
            if first_ref.is_none() && sha != "0000000000000000000000000000000000000000"
                && let Ok(bytes) = hex::decode(sha) {
                    first_ref = Some(bytes);
                }
        } else {
            // Non-ref pkt-line (e.g. version advertisement).
            let preview = std::str::from_utf8(&data).unwrap_or("<binary>").trim_end();
            debug!(preview, "pkt-line non-ref");
        }
    }

    // Fall back to the first non-zero ref (matches libgit2's list.first() behavior).
    first_ref.ok_or(SourceError::GitHashExtraction)
}

/// Parses a nix flake URL of the form `git+<scheme>://host/repo?rev=<hash>` into
/// `(git_url, rev)`.  The `git+` prefix is stripped so the returned URL is
/// suitable for direct use with libgit2.
fn parse_nix_git_url(nix_url: &str) -> Result<(String, String), SourceError> {
    let url = nix_url.strip_prefix("git+").unwrap_or(nix_url);
    let (base_url, query) = url.split_once('?').ok_or(SourceError::UrlParsing)?;
    let rev = query
        .split('&')
        .find_map(|p| p.strip_prefix("rev="))
        .ok_or(SourceError::MissingHash)?
        .to_string();

    Ok((base_url.to_string(), rev))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a git protocol v0 pkt-line from raw bytes.
    fn pkt_line(data: &[u8]) -> Vec<u8> {
        let len = data.len() + 4;
        let mut pkt = format!("{:04x}", len).into_bytes();
        pkt.extend_from_slice(data);
        pkt
    }

    /// Build a ref advertisement line: "<hex-sha1> <refname>\n"
    fn ref_line(hex_sha: &str, refname: &str) -> Vec<u8> {
        pkt_line(format!("{} {}\n", hex_sha, refname).as_bytes())
    }

    /// The first ref line includes NUL-separated capabilities:
    /// "<hex-sha1> <refname>\0<capabilities>\n"
    fn ref_line_with_caps(hex_sha: &str, refname: &str, caps: &str) -> Vec<u8> {
        let mut data = format!("{} {}\0{}\n", hex_sha, refname, caps).into_bytes();
        // pkt_line wraps it
        let len = data.len() + 4;
        let mut pkt = format!("{:04x}", len).into_bytes();
        pkt.append(&mut data);
        pkt
    }

    const FLUSH: &[u8] = b"0000";
    const FAKE_SHA: &str = "aabbccddee00112233445566778899aabbccddee";

    #[test]
    fn read_head_from_pktlines_basic() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "HEAD", "multi_ack"));
        buf.extend_from_slice(&ref_line(FAKE_SHA, "refs/heads/main"));
        buf.extend_from_slice(FLUSH);

        let result = read_head_from_pktlines(&mut buf.as_slice()).unwrap();
        assert_eq!(hex::encode(&result), FAKE_SHA);
    }

    #[test]
    fn read_head_from_pktlines_head_not_first() {
        let other_sha = "1111111111111111111111111111111111111111";
        let mut buf = Vec::new();
        buf.extend_from_slice(&ref_line_with_caps(other_sha, "refs/heads/main", "caps"));
        buf.extend_from_slice(&ref_line(FAKE_SHA, "HEAD"));
        buf.extend_from_slice(FLUSH);

        let result = read_head_from_pktlines(&mut buf.as_slice()).unwrap();
        assert_eq!(hex::encode(&result), FAKE_SHA);
    }

    #[test]
    fn read_head_from_pktlines_no_head_falls_back_to_first_ref() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "refs/heads/main", "caps"));
        buf.extend_from_slice(FLUSH);

        let result = read_head_from_pktlines(&mut buf.as_slice()).unwrap();
        assert_eq!(hex::encode(&result), FAKE_SHA);
    }

    #[test]
    fn read_head_from_pktlines_empty_repo_returns_error() {
        let zero_id = "0000000000000000000000000000000000000000";
        let mut buf = Vec::new();
        buf.extend_from_slice(&ref_line_with_caps(zero_id, "capabilities^{}", "multi_ack"));
        buf.extend_from_slice(FLUSH);

        let err = read_head_from_pktlines(&mut buf.as_slice()).unwrap_err();
        assert!(matches!(err, SourceError::GitHashExtraction));
    }

    /// Reproduces the original bug: git-daemon keeps the connection open after
    /// the ref advertisement. With `read_to_end` this would block until timeout
    /// and then fail with EAGAIN. With incremental pkt-line reading it should
    /// return HEAD immediately after the flush packet, without reading further.
    #[test]
    fn read_head_from_pktlines_server_keeps_connection_open() {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            // Send ref advertisement then flush — but do NOT close the connection.
            let mut payload = Vec::new();
            payload.extend_from_slice(&ref_line_with_caps(FAKE_SHA, "HEAD", "multi_ack"));
            payload.extend_from_slice(FLUSH);
            conn.write_all(&payload).unwrap();
            conn.flush().unwrap();
            // Keep connection open — sleep long enough that read_to_end would block.
            std::thread::sleep(std::time::Duration::from_secs(5));
            drop(conn);
        });

        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();

        // This must return quickly (not block for 2+ seconds waiting for EOF/timeout).
        let start = std::time::Instant::now();
        let result = read_head_from_pktlines(&mut stream).unwrap();
        let elapsed = start.elapsed();

        assert_eq!(hex::encode(&result), FAKE_SHA);
        assert!(
            elapsed.as_millis() < 1000,
            "read_head_from_pktlines blocked for {}ms — likely still using read_to_end",
            elapsed.as_millis()
        );

        drop(stream);
        server.join().unwrap();
    }
}
