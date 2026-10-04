/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use gradient_util::store_path::strip_store_prefix;
use gradient_util::sync::Mutex;
use gradient_wire::messages::ClientMessage;
use gradient_worker_client::connection::ProtoWriter;

#[derive(Clone, Default)]
pub struct PathVisibility {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    // `None` until the first handover: a worker without one sees every path.
    list: Mutex<Option<HashSet<String>>>,
    reports: Mutex<Option<ProtoWriter>>,
}

impl PathVisibility {
    pub fn allows(&self, store_path: &str) -> bool {
        self.inner
            .list
            .lock()
            .as_ref()
            .is_none_or(|list| list.contains(strip_store_prefix(store_path)))
    }

    pub fn start_list(&self) {
        *self.inner.list.lock() = Some(HashSet::new());
    }

    pub fn extend<I: IntoIterator<Item = String>>(&self, paths: I) {
        if let Some(list) = self.inner.list.lock().as_mut() {
            list.extend(paths);
        }
    }

    pub fn report_to(&self, writer: ProtoWriter) {
        *self.inner.reports.lock() = Some(writer);
    }

    pub async fn reveal(&self, store_paths: &[String]) -> Result<()> {
        let added: Vec<String> = {
            let mut list = self.inner.list.lock();
            let Some(list) = list.as_mut() else {
                return Ok(());
            };
            store_paths
                .iter()
                .map(|p| strip_store_prefix(p).to_owned())
                .filter(|p| list.insert(p.clone()))
                .collect()
        };
        let writer = self.inner.reports.lock().clone();
        match writer {
            Some(writer) if !added.is_empty() => {
                writer
                    .send(ClientMessage::PathsAdded { paths: added })
                    .await
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_list_every_path_is_visible() {
        let visibility = PathVisibility::default();
        assert!(visibility.allows("/nix/store/aaaa-hello"));
    }

    #[test]
    fn a_started_list_hides_every_path_not_on_it() {
        let visibility = PathVisibility::default();
        visibility.start_list();
        visibility.extend(["aaaa-hello".to_owned()]);
        assert!(visibility.allows("/nix/store/aaaa-hello"));
        assert!(!visibility.allows("/nix/store/bbbb-openssl"));
    }

    #[test]
    fn starting_a_new_list_forgets_the_previous_user() {
        let visibility = PathVisibility::default();
        visibility.start_list();
        visibility.extend(["aaaa-hello".to_owned()]);
        visibility.start_list();
        assert!(!visibility.allows("/nix/store/aaaa-hello"));
    }

    #[tokio::test]
    async fn a_revealed_path_becomes_visible() {
        let visibility = PathVisibility::default();
        visibility.start_list();
        visibility
            .reveal(&["/nix/store/bbbb-openssl".to_owned()])
            .await
            .unwrap();
        assert!(visibility.allows("/nix/store/bbbb-openssl"));
    }
}
