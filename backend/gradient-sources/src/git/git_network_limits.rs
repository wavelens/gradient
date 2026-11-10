/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::url::set_shallow_unless_local;
use git2::{FetchOptions, RemoteCallbacks};
use std::sync::Once;
use std::time::{Duration, Instant};
use tracing::{error, warn};

const FETCH_DEADLINE: Duration = Duration::from_secs(30 * 60);
const FETCH_BYTE_LIMIT: usize = 16 * 1024 * 1024 * 1024;
pub(super) const STALLED_SERVER_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn build_fetch_options_with_time_and_size_limits_and_optional_shallow(
    url: &str,
    shallow: bool,
) -> FetchOptions<'static> {
    let started = Instant::now();
    let mut callbacks = RemoteCallbacks::new();
    callbacks.transfer_progress(move |progress| {
        let reason = if started.elapsed() > FETCH_DEADLINE {
            "fetch deadline exceeded"
        } else if progress.received_bytes() > FETCH_BYTE_LIMIT {
            "The received byte count went past the fetch limit."
        } else {
            return true;
        };
        warn!(reason, "git fetch cancelled");
        false
    });

    let mut options = FetchOptions::new();
    options.remote_callbacks(callbacks);
    if shallow {
        set_shallow_unless_local(&mut options, url);
    }
    options
}

pub(super) fn set_libgit2_connect_and_read_timeouts_once() {
    static SET: Once = Once::new();
    SET.call_once(|| {
        let timeout = i32::try_from(STALLED_SERVER_TIMEOUT.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: the Once runs this before any libgit2 transport of this crate reads the options.
        let set = unsafe {
            git2::opts::set_server_connect_timeout_in_milliseconds(timeout)
                .and_then(|()| git2::opts::set_server_timeout_in_milliseconds(timeout))
        };
        if let Err(error) = set {
            error!(%error, "failed to set the timeout for the libgit2 server");
        }
    });
}
