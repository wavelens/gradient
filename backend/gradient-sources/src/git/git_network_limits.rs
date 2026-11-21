/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::https_credentials_for_operation_host::remote_callbacks_for_url_with_thread_https_credentials;
use super::url::set_shallow_unless_local;
use git2::FetchOptions;
use std::cell::RefCell;
use std::sync::Once;
use std::time::{Duration, Instant};
use tracing::{error, warn};

const FETCH_DEADLINE: Duration = Duration::from_secs(30 * 60);
const FETCH_BYTE_LIMIT: usize = 16 * 1024 * 1024 * 1024;
const PATH_FILTER_FETCH_DEADLINE: Duration = Duration::from_secs(120);
const PATH_FILTER_FETCH_TOTAL_BYTE_LIMIT: usize = 2 * 1024 * 1024 * 1024;
pub(super) const STALLED_SERVER_TIMEOUT: Duration = Duration::from_secs(60);

thread_local! {
    pub(super) static CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS:
        RefCell<Option<BlockingGitFetchOperationLimits>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub(crate) struct BlockingGitFetchOperationLimits {
    pub(super) deadline: Instant,
    pub(super) byte_limit: usize,
    pub(super) finished_fetch_bytes: usize,
}

impl BlockingGitFetchOperationLimits {
    pub(crate) fn ordinary_fetch_operation_limits() -> Self {
        Self::ending_after(FETCH_DEADLINE, FETCH_BYTE_LIMIT)
    }

    pub(crate) fn path_filter_fetch_operation_limits() -> Self {
        Self::ending_after(
            PATH_FILTER_FETCH_DEADLINE,
            PATH_FILTER_FETCH_TOTAL_BYTE_LIMIT,
        )
    }

    fn ending_after(duration: Duration, byte_limit: usize) -> Self {
        Self {
            deadline: Instant::now() + duration,
            byte_limit,
            finished_fetch_bytes: 0,
        }
    }

    fn fetch_deadline_or_byte_limit_exceeded_reason(
        &self,
        running_fetch_bytes: usize,
    ) -> Option<&'static str> {
        if Instant::now() > self.deadline {
            Some("fetch deadline exceeded")
        } else if self
            .finished_fetch_bytes
            .saturating_add(running_fetch_bytes)
            > self.byte_limit
        {
            Some("The received byte count went past the fetch limit.")
        } else {
            None
        }
    }
}

pub(super) fn blocking_git_fetch_limit_exceeded_reason_before_connect() -> Option<&'static str> {
    CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS.with_borrow(|limits| {
        limits
            .as_ref()?
            .fetch_deadline_or_byte_limit_exceeded_reason(0)
    })
}

pub(super) fn add_fetch_received_bytes_to_operation_total(received_bytes: usize) {
    CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS.with_borrow_mut(|limits| {
        if let Some(limits) = limits {
            limits.finished_fetch_bytes =
                limits.finished_fetch_bytes.saturating_add(received_bytes);
        }
    });
}

pub(super) fn current_thread_blocking_libgit2_fetch_time_and_byte_limits_or_ordinary_fetch_limits()
-> BlockingGitFetchOperationLimits {
    CURRENT_THREAD_BLOCKING_LIBGIT2_FETCH_LIMITS
        .with_borrow(Option::clone)
        .unwrap_or_else(BlockingGitFetchOperationLimits::ordinary_fetch_operation_limits)
}

pub(super) fn build_fetch_options_with_time_and_size_limits_and_optional_shallow(
    url: &str,
    shallow: bool,
) -> FetchOptions<'static> {
    let limits =
        current_thread_blocking_libgit2_fetch_time_and_byte_limits_or_ordinary_fetch_limits();
    let sideband_limits = limits.clone();
    let mut callbacks = remote_callbacks_for_url_with_thread_https_credentials(url);
    callbacks.transfer_progress(move |progress| {
        let Some(reason) =
            limits.fetch_deadline_or_byte_limit_exceeded_reason(progress.received_bytes())
        else {
            return true;
        };
        warn!(reason, "git fetch cancelled");
        false
    });
    callbacks.sideband_progress(move |_| {
        sideband_limits
            .fetch_deadline_or_byte_limit_exceeded_reason(0)
            .is_none()
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
