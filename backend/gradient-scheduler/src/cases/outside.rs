/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Mutex;
use std::time::Duration;

use gradient_entity::cache;
use tempfile::{NamedTempFile, TempDir};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::lock;
use super::shapes::store_path;

const SIGNING_HOST: &str = "http://upstream.test";
const SILENT_FOR: Duration = Duration::from_secs(3);
const NAR_HASH: &str = "sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf";

pub(crate) struct GitRepo {
    dir: TempDir,
    repo: git2::Repository,
    head: Mutex<Option<git2::Oid>>,
}

impl GitRepo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir for the repository");
        let repo = git2::Repository::init(dir.path()).expect("a repository");
        Self {
            dir,
            repo,
            head: Mutex::new(None),
        }
    }

    pub fn url(&self) -> String {
        format!("file://{}", self.dir.path().display())
    }

    pub fn commit(&self, message: &str) -> Vec<u8> {
        let author =
            git2::Signature::now("Test User", "test@example.invalid").expect("a commit author");
        let tree = self
            .repo
            .treebuilder(None)
            .and_then(|builder| builder.write())
            .and_then(|id| self.repo.find_tree(id))
            .expect("an empty tree");
        let mut head = lock(&self.head);
        let parent = head.and_then(|id| self.repo.find_commit(id).ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        let id = self
            .repo
            .commit(None, &author, &author, message, &tree, &parents)
            .expect("the commit is written");
        *head = Some(id);
        id.as_bytes().to_vec()
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
