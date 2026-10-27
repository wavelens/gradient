/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![allow(clippy::disallowed_methods, reason = "test SSH server")]

use crate::git::commit_info::{fetch_commit, head_refspec};
use crate::git::remote::ls_remote_head;
use russh::keys::{PrivateKey, PublicKey};
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

async fn serve_git_over_ssh(client_key: PublicKey) -> u16 {
    let config = Arc::new(russh::server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        keys: vec![decode(&ed25519_pem())],
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

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_is_listed_and_fetched_over_ssh_with_the_project_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.set_head("refs/heads/main").unwrap();
    commit(&repo, "first");
    let tip = commit(&repo, "second");

    let client_pem = ed25519_pem();
    let port = serve_git_over_ssh(decode(&client_pem).public_key().clone()).await;
    let ssh_url = format!("ssh://git@127.0.0.1:{port}{}", dir.path().display());

    let (head, fetched, wrong_key) = tokio::task::spawn_blocking(move || {
        (
            ls_remote_head(&ssh_url, Some(&client_pem), None),
            fetch_commit(&ssh_url, Some(&client_pem), &head_refspec(None)),
            ls_remote_head(&ssh_url, Some(&ed25519_pem()), None),
        )
    })
    .await
    .unwrap();

    let head = head.unwrap();
    assert_eq!(head.branch, "refs/heads/main");
    assert_eq!(head.commit, tip.as_bytes());
    assert_eq!(fetched.unwrap().hash, tip.as_bytes());
    assert!(wrong_key.is_err());
}
