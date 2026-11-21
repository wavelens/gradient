/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "This module is the only place in gradient-web for Argon2 calls."
)]

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use gradient_db::WebDb;
use gradient_types::ids::UserId;
use password_auth::{generate_hash, verify_password};
use rand::distr::{Alphanumeric, SampleString};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::client_ip::rate_limit_key_for_client_ip;
use crate::error::{WebError, WebResult};

const WRONG_PASSWORD_ATTEMPTS_BEFORE_WAIT: u32 = 5;
const MAX_WAIT_BETWEEN_PASSWORD_ATTEMPTS_SECONDS: u64 = 15 * 60;
const WRONG_PASSWORD_ENTRY_MAX_AGE_MILLISECONDS: u64 = 24 * 60 * 60 * 1000;
const PASSWORD_HASH_SLOT_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

static PASSWORD_HASH_FOR_MISSING_OR_PASSWORDLESS_ACCOUNT: LazyLock<String> =
    LazyLock::new(|| generate_hash(Alphanumeric.sample_string(&mut rand::rng(), 32)));

fn global_argon2_password_hashing_semaphore_permit_count() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().div_ceil(2))
}

static ARGON2_PASSWORD_HASH_JOB_SEMAPHORE: LazyLock<Arc<Semaphore>> = LazyLock::new(|| {
    Arc::new(Semaphore::new(
        global_argon2_password_hashing_semaphore_permit_count(),
    ))
});

async fn wait_for_hash_slot_with_semaphore(
    semaphore: Arc<Semaphore>,
) -> WebResult<OwnedSemaphorePermit> {
    Ok(
        tokio::time::timeout(PASSWORD_HASH_SLOT_WAIT_TIMEOUT, semaphore.acquire_owned())
            .await
            .map_err(|_| WebError::PasswordHashBusy {
                retry_after: PASSWORD_HASH_SLOT_WAIT_TIMEOUT,
            })?
            .context("semaphore for password hashing closed")?,
    )
}

pub(crate) async fn acquire_password_hashing_slot_with_five_second_timeout()
-> WebResult<OwnedSemaphorePermit> {
    wait_for_hash_slot_with_semaphore(Arc::clone(&ARGON2_PASSWORD_HASH_JOB_SEMAPHORE)).await
}

pub(crate) async fn compute_argon2_hash_for_new_password_on_blocking_pool_with_limited_semaphore_permit(
    password: String,
) -> WebResult<String> {
    let permit = acquire_password_hashing_slot_with_five_second_timeout().await?;
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        generate_hash(password)
    })
    .await
    .context("password hashing task failed")?)
}

pub(crate) async fn password_matches_account_hash_or_dummy_hash(
    permit: OwnedSemaphorePermit,
    password: String,
    stored_hash: Option<String>,
) -> WebResult<bool> {
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let matches = verify_password(
            &password,
            stored_hash
                .as_deref()
                .unwrap_or(&PASSWORD_HASH_FOR_MISSING_OR_PASSWORDLESS_ACCOUNT),
        )
        .is_ok();
        matches && stored_hash.is_some()
    })
    .await
    .context("password verification task failed")?)
}

fn wait_seconds_after_wrong_password_attempts(wrong_password_attempt_count: u32) -> u64 {
    match wrong_password_attempt_count.checked_sub(WRONG_PASSWORD_ATTEMPTS_BEFORE_WAIT) {
        None => 0,
        Some(wrong_password_attempts_beyond_free_allowance) => 2_u64
            .checked_pow(wrong_password_attempts_beyond_free_allowance)
            .map_or(MAX_WAIT_BETWEEN_PASSWORD_ATTEMPTS_SECONDS, |seconds| {
                seconds.min(MAX_WAIT_BETWEEN_PASSWORD_ATTEMPTS_SECONDS)
            }),
    }
}

struct WrongPasswordAttemptRecord {
    latest_password_attempt_sequence_number: u64,
    attempts_cleared_up_to_sequence_number: u64,
    last_wrong_password_attempt_instant: Instant,
}

impl WrongPasswordAttemptRecord {
    fn uncleared_password_attempt_count(&self) -> u32 {
        u32::try_from(
            self.latest_password_attempt_sequence_number
                - self.attempts_cleared_up_to_sequence_number,
        )
        .unwrap_or(u32::MAX)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordedPasswordAttempt {
    recorded_password_attempt_sequence_number: u64,
}

#[derive(Default)]
struct SignInAttemptRecordsAndHighestIssuedSequenceNumber {
    wrong_password_attempt_records_by_user_id: HashMap<UserId, WrongPasswordAttemptRecord>,
    highest_issued_password_attempt_sequence_number: u64,
}

impl SignInAttemptRecordsAndHighestIssuedSequenceNumber {
    fn record_password_attempt(
        &mut self,
        user_id: UserId,
        now: Instant,
    ) -> RecordedPasswordAttempt {
        let highest_issued = self.highest_issued_password_attempt_sequence_number;
        let record = self
            .wrong_password_attempt_records_by_user_id
            .entry(user_id)
            .or_insert(WrongPasswordAttemptRecord {
                latest_password_attempt_sequence_number: highest_issued,
                attempts_cleared_up_to_sequence_number: highest_issued,
                last_wrong_password_attempt_instant: now,
            });
        record.latest_password_attempt_sequence_number += 1;
        record.last_wrong_password_attempt_instant = now;
        self.highest_issued_password_attempt_sequence_number =
            highest_issued.max(record.latest_password_attempt_sequence_number);
        RecordedPasswordAttempt {
            recorded_password_attempt_sequence_number: record
                .latest_password_attempt_sequence_number,
        }
    }
}

#[derive(Default)]
pub(crate) struct WrongPasswordAttemptCountsAndLastAttemptTimesByUserId {
    sign_in_attempt_records_and_highest_issued_sequence_number:
        Mutex<SignInAttemptRecordsAndHighestIssuedSequenceNumber>,
}

impl WrongPasswordAttemptCountsAndLastAttemptTimesByUserId {
    /// Requests record an attempt under the lock for the wait check before the password check.
    /// Parallel requests can't all pass. Callers hold a hash slot first, so requests that return 503
    /// record nothing. Wrong passwords, errors and cancellations keep the record and count toward
    /// the wait. Correct passwords clear only the attempts up to their own. Later attempts still
    /// count toward the wait.
    pub(crate) fn check_wait_and_record_password_attempt(
        &self,
        user_id: UserId,
        read_current_time: impl FnOnce() -> Instant,
    ) -> Result<RecordedPasswordAttempt, Duration> {
        let mut state = self.lock_sign_in_attempt_records();
        let now = read_current_time();
        if let Some(record) = state
            .wrong_password_attempt_records_by_user_id
            .get(&user_id)
            && let Some(remaining) = Duration::from_secs(
                wait_seconds_after_wrong_password_attempts(
                    record.uncleared_password_attempt_count(),
                ),
            )
            .checked_sub(now.saturating_duration_since(record.last_wrong_password_attempt_instant))
            .filter(|remaining| !remaining.is_zero())
        {
            return Err(remaining);
        }
        Ok(state.record_password_attempt(user_id, now))
    }

    pub(crate) fn record_wrong_password_attempt(
        &self,
        user_id: UserId,
        read_current_time: impl FnOnce() -> Instant,
    ) -> RecordedPasswordAttempt {
        let mut state = self.lock_sign_in_attempt_records();
        let now = read_current_time();
        state.record_password_attempt(user_id, now)
    }

    pub(crate) fn remove_wrong_sign_in_attempts_through_recorded_attempt(
        &self,
        user_id: UserId,
        recorded_attempt: RecordedPasswordAttempt,
    ) {
        let mut state = self.lock_sign_in_attempt_records();
        let Entry::Occupied(mut occupied) = state
            .wrong_password_attempt_records_by_user_id
            .entry(user_id)
        else {
            return;
        };
        let record = occupied.get_mut();
        record.attempts_cleared_up_to_sequence_number = record
            .attempts_cleared_up_to_sequence_number
            .max(recorded_attempt.recorded_password_attempt_sequence_number);
        if record.uncleared_password_attempt_count() == 0 {
            occupied.remove();
        }
    }

    pub(crate) fn remove_wrong_attempt_entries_older_than_24_hours(&self, now: Instant) {
        let mut state = self.lock_sign_in_attempt_records();
        let records = &mut state.wrong_password_attempt_records_by_user_id;
        records.retain(|_, record| {
            now.saturating_duration_since(record.last_wrong_password_attempt_instant)
                < Duration::from_millis(WRONG_PASSWORD_ENTRY_MAX_AGE_MILLISECONDS)
        });
        records.shrink_to_fit();
    }

    fn lock_sign_in_attempt_records(
        &self,
    ) -> std::sync::MutexGuard<'_, SignInAttemptRecordsAndHighestIssuedSequenceNumber> {
        self.sign_in_attempt_records_and_highest_issued_sequence_number
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) async fn record_password_attempt_or_refuse_with_429_when_wait_is_due_unless_client_network_has_same_account_live_session(
        &self,
        db: &WebDb,
        user_id: UserId,
        client_ip: Option<&str>,
    ) -> WebResult<RecordedPasswordAttempt> {
        let retry_after = match self.check_wait_and_record_password_attempt(user_id, Instant::now) {
            Ok(recorded_attempt) => return Ok(recorded_attempt),
            Err(retry_after) => retry_after,
        };
        let live_session_ips = gradient_db::sessions::usable_browser_session_ip_addresses_of_user(
            db,
            user_id,
            gradient_types::now(),
        )
        .await?;
        let client_ip = client_ip.and_then(|ip| ip.parse().ok());
        if client_ip.is_some_and(|client_ip| {
            client_network_matches_live_session_network(
                client_ip,
                live_session_ips.iter().map(String::as_str),
            )
        }) {
            Ok(self.record_wrong_password_attempt(user_id, Instant::now))
        } else {
            Err(WebError::SignInBusy { retry_after })
        }
    }
}

/// A sign-in from a network with a live browser session of the same account skips the wait, so
/// wrong passwords cannot lock out a signed-in user. Sessions of a CLI sign-in approved in the
/// browser give no exemption, because a stolen browser session can approve such a sign-in from any
/// network. The session network comes from the TCP peer address, with proxy headers only from
/// trusted proxies, so a request cannot fake it.
pub(crate) fn client_network_matches_live_session_network<'a>(
    client_ip: IpAddr,
    live_sign_in_session_ip_addresses: impl IntoIterator<Item = &'a str>,
) -> bool {
    let client_ip_rate_limit_network = rate_limit_key_for_client_ip(client_ip);
    live_sign_in_session_ip_addresses
        .into_iter()
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .any(|address| rate_limit_key_for_client_ip(address) == client_ip_rate_limit_network)
}

#[cfg(test)]
mod tests;
