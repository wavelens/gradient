/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use anyhow::{Context, Result, bail};
use gradient_wire::types::JobContext;

use crate::executor::AbortSignal;
use crate::proto::credentials::CredentialStore;

pub async fn checkout_work(
    work: &Path,
    context: &JobContext,
    credentials: &CredentialStore,
    abort: &mut AbortSignal,
) -> Result<()> {
    if abort.is_aborted() {
        bail!("job aborted before the checkout");
    }
    let ssh_key = project_ssh_key(credentials)?;
    let (dir, url, commit) = (
        work.to_path_buf(),
        context.repository.clone(),
        context.commit.clone(),
    );
    let clone = tokio::task::spawn_blocking(move || {
        gradient_sources::checkout_commit_into(&dir, &url, &commit, ssh_key.as_deref())
    });
    tokio::select! {
        biased;
        () = abort.aborted() => bail!("job aborted during the checkout"),
        result = clone => result.context("checkout task panicked")?,
    }
}

fn project_ssh_key(credentials: &CredentialStore) -> Result<Option<String>> {
    credentials
        .ssh_key()
        .map(|key| String::from_utf8(key.expose().to_vec()).context("project SSH key is not UTF-8"))
        .transpose()
}
