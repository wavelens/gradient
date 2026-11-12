/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![allow(clippy::disallowed_methods, reason = "test SSH server")]

use super::{SshGitPrivateKeyKnownHostsAndObservations, fetch_ssh_host_key};
use crate::git::commit_info::{fetch_commit, head_refspec};
use crate::git::remote::ls_remote_head;
use gradient_wire::messages::{GitServerSshHostKey, GitSshHostKeyObservation};
use russh::keys::{Algorithm, EcdsaCurve, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Handle, Msg, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use ssh_key::LineEnding;
use ssh_key::private::Ed25519Keypair;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

fn ed25519_pem() -> String {
    let pair = ed25519_compact::KeyPair::generate();
    ssh_key::PrivateKey::from(Ed25519Keypair::from_seed(&pair.sk.seed()))
        .to_openssh(LineEnding::LF)
        .unwrap()
        .to_string()
}

fn decode(pem: &str) -> PrivateKey {
    russh::keys::decode_secret_key(pem, None).unwrap()
}

struct GitServer {
    client_key: PublicKey,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for GitServer {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        if user == "git" && key.key_data() == self.client_key.key_data() {
            return Ok(Auth::Accept);
        }

        Ok(Auth::reject())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(channel) = self.channels.remove(&id) else {
            return session.channel_failure(id);
        };

        session.channel_success(id)?;
        let command = String::from_utf8_lossy(data).into_owned();
        tokio::spawn(run_in_shell(channel, session.handle(), command));
        Ok(())
    }
}

async fn run_in_shell(channel: Channel<Msg>, handle: Handle, command: String) {
    let id = channel.id();
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (mut from_client, mut to_client) = tokio::io::split(channel.into_stream());
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let input = tokio::spawn(async move { tokio::io::copy(&mut from_client, &mut stdin).await });

    let _ = tokio::io::copy(&mut stdout, &mut to_client).await;
    let _ = to_client.flush().await;
    let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(1);
    input.abort();
    let _ = handle
        .exit_status_request(id, u32::try_from(code).unwrap_or(1))
        .await;
    let _ = handle.eof(id).await;
    let _ = handle.close(id).await;
}

async fn serve_git_over_ssh(client_key: PublicKey, host_keys: Vec<PrivateKey>) -> u16 {
    let config = Arc::new(russh::server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        keys: host_keys,
        preferred: gradient_util::ssh::ALGORITHMS,
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let handler = GitServer {
                client_key: client_key.clone(),
                channels: HashMap::new(),
            };
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
            });
        }
    });

    port
}

fn generate_random_ssh_host_private_key(algorithm: Algorithm) -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), algorithm).unwrap()
}

fn make_saved_git_server_ssh_host_key_for_local_test_server(
    port: u16,
    key: &PrivateKey,
) -> GitServerSshHostKey {
    GitServerSshHostKey {
        host: "127.0.0.1".into(),
        port,
        public_key: key.public_key().to_openssh().unwrap(),
    }
}

fn commit(repo: &git2::Repository, message: &str) -> git2::Oid {
    let sig = git2::Signature::now("Ada", "ada@example.com").unwrap();
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .unwrap()
}

fn create_temp_git_repository_with_two_commits_on_main() -> (tempfile::TempDir, git2::Oid) {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    commit(&repo, "first");
    let tip = commit(&repo, "second");
    (dir, tip)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_is_listed_and_fetched_over_ssh_with_the_project_key() {
    let (dir, tip) = create_temp_git_repository_with_two_commits_on_main();
    let client_pem = ed25519_pem();
    let host_key = generate_random_ssh_host_private_key(Algorithm::Ed25519);
    let port = serve_git_over_ssh(
        decode(&client_pem).public_key().clone(),
        vec![host_key.clone()],
    )
    .await;
    let ssh_url = format!("ssh://git@127.0.0.1:{port}{}", dir.path().display());
    let saved = vec![make_saved_git_server_ssh_host_key_for_local_test_server(
        port, &host_key,
    )];
    let ssh = SshGitPrivateKeyKnownHostsAndObservations::new(client_pem.into(), saved.clone());
    let foreign = SshGitPrivateKeyKnownHostsAndObservations::new(ed25519_pem().into(), saved);

    let (head, fetched, wrong_key) = tokio::task::spawn_blocking({
        let ssh = ssh.clone();
        move || {
            (
                ls_remote_head(&ssh_url, Some(&ssh), None),
                fetch_commit(&ssh_url, Some(&ssh), &head_refspec(None)),
                ls_remote_head(&ssh_url, Some(&foreign), None),
            )
        }
    })
    .await
    .unwrap();

    let head = head.unwrap();
    assert_eq!(head.branch, "refs/heads/main");
    assert_eq!(head.commit, tip.as_bytes());
    assert_eq!(fetched.unwrap().hash, tip.as_bytes());
    assert!(wrong_key.is_err());
    assert!(ssh.observations().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_host_is_accepted_and_reported_as_learned() {
    let (dir, _) = create_temp_git_repository_with_two_commits_on_main();
    let client_pem = ed25519_pem();
    let host_key = generate_random_ssh_host_private_key(Algorithm::Ed25519);
    let port = serve_git_over_ssh(
        decode(&client_pem).public_key().clone(),
        vec![host_key.clone()],
    )
    .await;
    let ssh_url = format!("ssh://git@127.0.0.1:{port}{}", dir.path().display());
    let ssh = SshGitPrivateKeyKnownHostsAndObservations::new(client_pem.into(), Vec::new());

    let head = tokio::task::spawn_blocking({
        let ssh = ssh.clone();
        move || ls_remote_head(&ssh_url, Some(&ssh), None)
    })
    .await
    .unwrap();

    assert!(head.is_ok(), "{:?}", head.err());
    assert_eq!(
        ssh.observations(),
        vec![GitSshHostKeyObservation::UnknownHostKeyAcceptedForSaving {
            key: make_saved_git_server_ssh_host_key_for_local_test_server(port, &host_key),
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_host_key_is_accepted_silently() {
    let (dir, tip) = create_temp_git_repository_with_two_commits_on_main();
    let client_pem = ed25519_pem();
    let host_key = generate_random_ssh_host_private_key(Algorithm::Ed25519);
    let port = serve_git_over_ssh(
        decode(&client_pem).public_key().clone(),
        vec![host_key.clone()],
    )
    .await;
    let ssh_url = format!("ssh://git@LocalHost:{port}{}", dir.path().display());
    let saved = GitServerSshHostKey {
        host: "localhost".into(),
        ..make_saved_git_server_ssh_host_key_for_local_test_server(port, &host_key)
    };
    let ssh = SshGitPrivateKeyKnownHostsAndObservations::new(client_pem.into(), vec![saved]);

    let (head, fetched) = tokio::task::spawn_blocking({
        let ssh = ssh.clone();
        move || {
            (
                ls_remote_head(&ssh_url, Some(&ssh), None),
                fetch_commit(&ssh_url, Some(&ssh), &head_refspec(None)),
            )
        }
    })
    .await
    .unwrap();

    assert!(head.is_ok(), "{:?}", head.err());
    assert_eq!(fetched.unwrap().hash, tip.as_bytes());
    assert!(ssh.observations().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_different_host_key_is_refused_and_reported_as_changed() {
    let (dir, _) = create_temp_git_repository_with_two_commits_on_main();
    let client_pem = ed25519_pem();
    let host_key = generate_random_ssh_host_private_key(Algorithm::Ed25519);
    let port = serve_git_over_ssh(decode(&client_pem).public_key().clone(), vec![host_key]).await;
    let ssh_url = format!("ssh://git@127.0.0.1:{port}{}", dir.path().display());
    let ssh = SshGitPrivateKeyKnownHostsAndObservations::new(
        client_pem.into(),
        vec![make_saved_git_server_ssh_host_key_for_local_test_server(
            port,
            &generate_random_ssh_host_private_key(Algorithm::Ed25519),
        )],
    );

    let head = tokio::task::spawn_blocking({
        let ssh = ssh.clone();
        move || ls_remote_head(&ssh_url, Some(&ssh), None)
    })
    .await
    .unwrap();

    let error = head.err().unwrap().to_string();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(
        ssh.observations(),
        vec![GitSshHostKeyObservation::HostKeyMismatchConnectionRefused {
            host: "127.0.0.1".into(),
            port,
        }]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_ecdsa_key_still_matches_when_the_host_also_offers_ed25519() {
    let (dir, _) = create_temp_git_repository_with_two_commits_on_main();
    let client_pem = ed25519_pem();
    let ecdsa = generate_random_ssh_host_private_key(Algorithm::Ecdsa {
        curve: EcdsaCurve::NistP256,
    });
    let port = serve_git_over_ssh(
        decode(&client_pem).public_key().clone(),
        vec![
            generate_random_ssh_host_private_key(Algorithm::Ed25519),
            ecdsa.clone(),
        ],
    )
    .await;
    let ssh_url = format!("ssh://git@127.0.0.1:{port}{}", dir.path().display());
    let ssh = SshGitPrivateKeyKnownHostsAndObservations::new(
        client_pem.into(),
        vec![make_saved_git_server_ssh_host_key_for_local_test_server(
            port, &ecdsa,
        )],
    );

    let head = tokio::task::spawn_blocking({
        let ssh = ssh.clone();
        move || ls_remote_head(&ssh_url, Some(&ssh), None)
    })
    .await
    .unwrap();

    assert!(head.is_ok(), "{:?}", head.err());
    assert!(ssh.observations().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_presented_host_key_can_be_fetched_without_a_saved_one() {
    let host_key = generate_random_ssh_host_private_key(Algorithm::Ed25519);
    let port = serve_git_over_ssh(
        decode(&ed25519_pem()).public_key().clone(),
        vec![host_key.clone()],
    )
    .await;

    let presented = fetch_ssh_host_key("127.0.0.1", port).await.unwrap();

    assert_eq!(presented, host_key.public_key().to_openssh().unwrap());
}
