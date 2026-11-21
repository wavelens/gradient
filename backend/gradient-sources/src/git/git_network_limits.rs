/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::url::set_shallow_unless_local;
use git2::{FetchOptions, RemoteCallbacks};
use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};
use tracing::warn;

const FETCH_DEADLINE: Duration = Duration::from_secs(30 * 60);
const FETCH_BYTE_LIMIT: usize = 16 * 1024 * 1024 * 1024;
const PATH_FILTER_FETCH_DEADLINE: Duration = Duration::from_secs(120);
const PATH_FILTER_FETCH_TOTAL_BYTE_LIMIT: usize = 2 * 1024 * 1024 * 1024;
pub(super) const STALLED_SERVER_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum RefAdvertisementEnd {
    RefAdvertisementIsWholeStream,
    AdvertisementEndsAtFirstFlushPacket,
    NoRefAdvertisementInStream,
}

pub(super) struct GitNetworkStreamReadDeadlineAndByteLimitChecker<S> {
    inner: S,
    end: RefAdvertisementEnd,
    byte_limit: usize,
    counted_bytes: usize,
    packet_length_digits: Vec<u8>,
    unread_packet_payload_bytes: usize,
    advertisement_finished: bool,
    fetch_operation_limits: BlockingGitFetchOperationLimits,
    total_bytes_read_from_git_network: usize,
}

impl<S> GitNetworkStreamReadDeadlineAndByteLimitChecker<S> {
    pub(super) fn new(inner: S, end: RefAdvertisementEnd) -> Self {
        Self::with_byte_limit(inner, end, MAX_GIT_REF_ADVERTISEMENT_READ_BYTE_COUNT)
    }

    pub(super) fn with_byte_limit(inner: S, end: RefAdvertisementEnd, byte_limit: usize) -> Self {
        Self {
            inner,
            end,
            byte_limit,
            counted_bytes: 0,
            packet_length_digits: Vec::with_capacity(4),
            unread_packet_payload_bytes: 0,
            advertisement_finished: matches!(end, RefAdvertisementEnd::NoRefAdvertisementInStream),
            fetch_operation_limits:
                current_thread_blocking_libgit2_fetch_time_and_byte_limits_or_ordinary_fetch_limits(
                ),
            total_bytes_read_from_git_network: 0,
        }
    }

    fn advertisement_byte_count_in(&mut self, bytes: &[u8]) -> usize {
        if let RefAdvertisementEnd::RefAdvertisementIsWholeStream = self.end {
            return bytes.len();
        }
        let mut index = 0;
        while index < bytes.len() && !self.advertisement_finished {
            if self.unread_packet_payload_bytes > 0 {
                let step = self.unread_packet_payload_bytes.min(bytes.len() - index);
                self.unread_packet_payload_bytes -= step;
                index += step;
                continue;
            }
            self.packet_length_digits.push(bytes[index]);
            index += 1;
            if self.packet_length_digits.len() < 4 {
                continue;
            }
            let packet_length = std::str::from_utf8(&self.packet_length_digits)
                .ok()
                .and_then(|digits| usize::from_str_radix(digits, 16).ok());
            self.packet_length_digits.clear();
            match packet_length {
                Some(length) if length >= 4 => self.unread_packet_payload_bytes = length - 4,
                Some(_) => self.advertisement_finished = true,
                None => self.unread_packet_payload_bytes = usize::MAX,
            }
        }
        index
    }
}

impl<S: Read> Read for GitNetworkStreamReadDeadlineAndByteLimitChecker<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.total_bytes_read_from_git_network =
            self.total_bytes_read_from_git_network.saturating_add(read);
        if let Some(reason) = self
            .fetch_operation_limits
            .fetch_deadline_or_byte_limit_exceeded_reason(self.total_bytes_read_from_git_network)
        {
            return Err(io::Error::other(reason));
        }
        if self.advertisement_finished {
            return Ok(read);
        }
        let counted = self.advertisement_byte_count_in(&buf[..read]);
        self.counted_bytes = self.counted_bytes.saturating_add(counted);
        if self.counted_bytes > self.byte_limit {
            return Err(io::Error::other(format!(
                "ref advertisement larger than {} MiB",
                self.byte_limit / (1024 * 1024)
            )));
        }
        Ok(read)
    }
}

impl<S: Write> Write for GitNetworkStreamReadDeadlineAndByteLimitChecker<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

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
    let mut callbacks = RemoteCallbacks::new();
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
