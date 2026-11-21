/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Mutex;
use std::time::Duration;

use gradient_entity::cache;
use gradient_test_support::git_daemon_and_fork_repository_fixtures::{
    GitDaemonProcessGuard, start_git_daemon_on_free_loopback_port_serving_base_directory,
};
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::lock;
use super::shapes::store_path;

const SIGNING_HOST: &str = "http://upstream.test";
const SILENT_FOR: Duration = Duration::from_secs(3);
const NAR_HASH: &str = "sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf";

pub(crate) struct GitRepo {
    _daemon: GitDaemonProcessGuard,
    base_url: String,
    repo: git2::Repository,
    head: Mutex<Option<git2::Oid>>,
    _dir: TempDir,
}

impl GitRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir for the repository");
        let repo = git2::Repository::init(dir.path().join("repo")).expect("a repository");
        repo.config()
            .and_then(|mut config| config.set_bool("uploadpack.allowReachableSHA1InWant", true))
            .expect("the repository serves reachable commits by hash");
        let (daemon, base_url) =
            start_git_daemon_on_free_loopback_port_serving_base_directory(dir.path());
        Self {
            _daemon: daemon,
            base_url,
            repo,
            head: Mutex::new(None),
            _dir: dir,
        }
    }

    pub fn url(&self) -> String {
        format!("{}/repo", self.base_url)
    }

    pub fn commit(&self, message: &str) -> Vec<u8> {
        self.commit_files(message, &[])
    }

    pub fn commit_files(&self, message: &str, paths: &[&str]) -> Vec<u8> {
        let author =
            git2::Signature::now("Test User", "test@example.invalid").expect("a commit author");
        let mut head = lock(&self.head);
        let parent = head.and_then(|id| self.repo.find_commit(id).ok());
        let parent_tree = match &parent {
            Some(commit) => commit.tree().expect("the tree of the parent"),
            None => self
                .repo
                .treebuilder(None)
                .and_then(|builder| builder.write())
                .and_then(|id| self.repo.find_tree(id))
                .expect("an empty tree"),
        };
        let mut update = git2::build::TreeUpdateBuilder::new();
        for path in paths {
            let blob = self
                .repo
                .blob(message.as_bytes())
                .expect("the file content is written");
            update.upsert(*path, blob, git2::FileMode::Blob);
        }
        let tree = update
            .create_updated(&self.repo, &parent_tree)
            .and_then(|id| self.repo.find_tree(id))
            .expect("the tree with the changed files");
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        let id = self
            .repo
            .commit(None, &author, &author, message, &tree, &parents)
            .expect("the commit is written");
        *head = Some(id);
        id.as_bytes().to_vec()
    }

    pub fn delete_commit_object(&self, commit: &[u8]) {
        let commit_id_hex_string = git2::Oid::from_bytes(commit)
            .expect("a commit id")
            .to_string();
        let (loose_object_subdirectory_name, loose_object_file_name) =
            commit_id_hex_string.split_at(2);
        std::fs::remove_file(
            self.repo
                .path()
                .join("objects")
                .join(loose_object_subdirectory_name)
                .join(loose_object_file_name),
        )
        .expect("delete a loose commit object");
    }

    pub fn push(&self, branch: &str, commit: &[u8]) {
        let id = git2::Oid::from_bytes(commit).expect("a commit id");
        let reference = format!("refs/heads/{branch}");
        self.repo
            .reference(&reference, id, true, "push")
            .expect("the branch moves");
        self.repo
            .set_head(&reference)
            .expect("HEAD names the branch");
    }
}

pub(crate) struct UpstreamServer {
    server: MockServer,
    secret: NamedTempFile,
    signer: cache::Model,
    stranger: cache::Model,
    pub public_key: String,
}

impl UpstreamServer {
    pub async fn start() -> Self {
        let (secret, path) = gradient_graph::testing::secret_file();
        let signer = signing_cache(&path, "upstream");
        let stranger = signing_cache(&path, "upstream");
        let public_key =
            gradient_sources::format_cache_public_key(&path, signer.clone(), SIGNING_HOST.into())
                .expect("the public key of the upstream");
        Self {
            server: MockServer::start().await,
            secret,
            signer,
            stranger,
            public_key,
        }
    }

    pub fn url(&self) -> String {
        self.server.uri()
    }

    pub async fn hit(&self, hash: &str, package: &str, references: &[(&str, &str)]) {
        let body = self.narinfo(hash, package, references, Some(&self.signer));
        self.answer(hash, ResponseTemplate::new(200).set_body_string(body))
            .await;
    }

    pub async fn unsigned(&self, hash: &str, package: &str) {
        let body = self.narinfo(hash, package, &[], None);
        self.answer(hash, ResponseTemplate::new(200).set_body_string(body))
            .await;
    }

    pub async fn wrongly_signed(&self, hash: &str, package: &str) {
        let body = self.narinfo(hash, package, &[], Some(&self.stranger));
        self.answer(hash, ResponseTemplate::new(200).set_body_string(body))
            .await;
    }

    pub async fn miss(&self, hash: &str) {
        self.answer(hash, ResponseTemplate::new(404)).await;
    }

    pub async fn error(&self, hash: &str) {
        self.answer(hash, ResponseTemplate::new(500)).await;
    }

    pub async fn silent(&self, hash: &str) {
        self.answer(hash, ResponseTemplate::new(404).set_delay(SILENT_FOR))
            .await;
    }

    pub async fn forget_answers(&self) {
        self.server.reset().await;
    }

    async fn answer(&self, hash: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(format!("/{hash}.narinfo")))
            .respond_with(response)
            .mount(&self.server)
            .await;
    }

    fn narinfo(
        &self,
        hash: &str,
        package: &str,
        references: &[(&str, &str)],
        signer: Option<&cache::Model>,
    ) -> String {
        let path = store_path(hash, package);
        let references: Vec<String> = references
            .iter()
            .map(|(hash, package)| format!("{hash}-{package}"))
            .collect();
        let mut body = format!(
            "StorePath: {path}\nURL: nar/{hash}.nar\nCompression: none\nNarHash: {NAR_HASH}\n\
             NarSize: 5\nFileHash: {NAR_HASH}\nFileSize: 5\nReferences: {}\n",
            references.join(" ")
        );
        if let Some(signer) = signer {
            let sig = gradient_sources::sign_narinfo_fingerprint(
                &self.secret.path().to_string_lossy(),
                signer.clone(),
                SIGNING_HOST.into(),
                &path,
                NAR_HASH,
                5,
                &references,
            )
            .expect("the narinfo is signed");
            body.push_str(&format!("Sig: {sig}\n"));
        }
        body
    }
}

fn signing_cache(secret: &str, name: &str) -> cache::Model {
    let (private_key, public_key) =
        gradient_sources::generate_signing_key(secret).expect("a signing key");
    cache::Model {
        name: name.into(),
        private_key,
        public_key,
        ..Default::default()
    }
}
