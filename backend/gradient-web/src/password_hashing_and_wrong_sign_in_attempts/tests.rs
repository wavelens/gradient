/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;

fn build_wrong_sign_in_attempts_map_for_user_at(
    user_id_under_test: UserId,
    wrong_password_attempt_count: u32,
    now: Instant,
) -> WrongPasswordAttemptCountsAndLastAttemptTimesByUserId {
    let map = WrongPasswordAttemptCountsAndLastAttemptTimesByUserId::default();
    for _ in 0..wrong_password_attempt_count {
        map.record_wrong_password_attempt(user_id_under_test, || now);
    }
    map
}

async fn acquire_private_password_hashing_slot() -> OwnedSemaphorePermit {
    wait_for_hash_slot_with_semaphore(Arc::new(Semaphore::new(1)))
        .await
        .unwrap()
}

#[tokio::test]
async fn password_hash_verification_accepts_correct_password_and_rejects_wrong_password_or_missing_account_hash()
 {
    let hash = generate_hash("correct password");

    assert!(
        password_matches_account_hash_or_dummy_hash(
            acquire_private_password_hashing_slot().await,
            "correct password".to_owned(),
            Some(hash.clone())
        )
        .await
        .unwrap()
    );
    assert!(
        !password_matches_account_hash_or_dummy_hash(
            acquire_private_password_hashing_slot().await,
            "wrong password".to_owned(),
            Some(hash)
        )
        .await
        .unwrap()
    );
    assert!(
        !password_matches_account_hash_or_dummy_hash(
            acquire_private_password_hashing_slot().await,
            "correct password".to_owned(),
            None
        )
        .await
        .unwrap()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn password_verification_runs_on_blocking_pool_without_stopping_single_threaded_runtime() {
    let hash = generate_hash("correct password");
    let password_verification_task = password_matches_account_hash_or_dummy_hash(
        acquire_private_password_hashing_slot().await,
        "correct password".to_owned(),
        Some(hash),
    );
    tokio::pin!(password_verification_task);

    let mut other_task_yield_count = 0_u32;
    let matches = loop {
        tokio::select! {
            biased;
            matches = &mut password_verification_task => break matches.unwrap(),
            () = tokio::task::yield_now() => other_task_yield_count += 1,
        }
    };

    assert!(matches);
    assert!(
        other_task_yield_count > 10,
        "{other_task_yield_count} yields"
    );
}

#[test]
fn test_wait_seconds_after_wrong_password_attempts_increases_and_is_capped_at_fifteen_minutes() {
    assert_eq!(wait_seconds_after_wrong_password_attempts(0), 0);
    assert_eq!(wait_seconds_after_wrong_password_attempts(4), 0);
    assert_eq!(wait_seconds_after_wrong_password_attempts(5), 1);
    assert_eq!(wait_seconds_after_wrong_password_attempts(6), 2);
    assert_eq!(wait_seconds_after_wrong_password_attempts(10), 32);
    assert_eq!(wait_seconds_after_wrong_password_attempts(14), 512);
    assert_eq!(wait_seconds_after_wrong_password_attempts(15), 900);
    assert_eq!(wait_seconds_after_wrong_password_attempts(u32::MAX), 900);
}

#[test]
fn test_remaining_wait_before_password_retry_allowed_after_six_wrong_attempts_counts_down_from_last_attempt_and_ends_after_two_seconds()
 {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map =
        build_wrong_sign_in_attempts_map_for_user_at(user_id_under_test, 6, injected_time_start);

    assert_eq!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start),
        Err(Duration::from_secs(2))
    );
    assert_eq!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start
            + Duration::from_millis(1500)),
        Err(Duration::from_millis(500))
    );
    assert!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start
            + Duration::from_secs(2))
            .is_ok()
    );
    assert!(
        map.check_wait_and_record_password_attempt(UserId::new(uuid::Uuid::now_v7()), || {
            injected_time_start
        })
        .is_ok()
    );
}

fn run_threads_calling_check_wait_and_record_password_attempt_and_count_allowed(
    map: &WrongPasswordAttemptCountsAndLastAttemptTimesByUserId,
    user_id_under_test: UserId,
    now: Instant,
    concurrent_attempt_thread_count: usize,
) -> usize {
    let reservation_start_barrier = std::sync::Barrier::new(concurrent_attempt_thread_count);
    std::thread::scope(|scope| {
        let password_attempt_thread_join_handles: Vec<_> = (0..concurrent_attempt_thread_count)
            .map(|_| {
                scope.spawn(|| {
                    reservation_start_barrier.wait();
                    map.check_wait_and_record_password_attempt(user_id_under_test, || now)
                        .is_ok()
                })
            })
            .collect();
        password_attempt_thread_join_handles
            .into_iter()
            .map(|password_attempt_thread_join_handle| {
                password_attempt_thread_join_handle.join().unwrap()
            })
            .filter(|&allowed| allowed)
            .count()
    })
}

#[test]
fn test_parallel_password_attempt_reservations_allow_exactly_five_and_wait_for_rest() {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map = WrongPasswordAttemptCountsAndLastAttemptTimesByUserId::default();

    assert_eq!(
        run_threads_calling_check_wait_and_record_password_attempt_and_count_allowed(
            &map,
            user_id_under_test,
            injected_time_start,
            16
        ),
        5
    );
}

#[test]
fn exactly_one_parallel_password_attempt_reservation_is_allowed_after_wait_ended_with_more_than_five_earlier_wrong_attempts()
 {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map =
        build_wrong_sign_in_attempts_map_for_user_at(user_id_under_test, 7, injected_time_start);

    assert_eq!(
        run_threads_calling_check_wait_and_record_password_attempt_and_count_allowed(
            &map,
            user_id_under_test,
            injected_time_start + Duration::from_secs(4),
            16
        ),
        1
    );
}

#[test]
fn clearing_attempts_after_correct_password_allows_next_reservation_without_wait() {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map = WrongPasswordAttemptCountsAndLastAttemptTimesByUserId::default();
    let five_attempts_before_correct_password: Vec<_> = (0..5)
        .map(|_| {
            map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
                .unwrap()
        })
        .collect();
    assert_eq!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start),
        Err(Duration::from_secs(1))
    );

    map.remove_wrong_sign_in_attempts_through_recorded_attempt(
        user_id_under_test,
        five_attempts_before_correct_password[4],
    );

    assert!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
            .is_ok()
    );
}

#[test]
fn correct_password_for_one_request_does_not_clear_attempts_from_other_requests() {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map = WrongPasswordAttemptCountsAndLastAttemptTimesByUserId::default();
    let correct_password_request_attempt = map
        .check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
        .unwrap();
    for attempt_delay_milliseconds in 1..=2 {
        assert!(
            map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start
                + Duration::from_millis(attempt_delay_milliseconds))
                .is_ok()
        );
    }

    map.remove_wrong_sign_in_attempts_through_recorded_attempt(
        user_id_under_test,
        correct_password_request_attempt,
    );

    let time_after_other_request_attempts = injected_time_start + Duration::from_millis(3);
    for _ in 0..3 {
        assert!(
            map.check_wait_and_record_password_attempt(user_id_under_test, || {
                time_after_other_request_attempts
            })
            .is_ok()
        );
    }
    assert_eq!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || {
            time_after_other_request_attempts
        }),
        Err(Duration::from_secs(1))
    );
}

fn assert_exactly_two_more_attempts_pass_before_wait(
    map: &WrongPasswordAttemptCountsAndLastAttemptTimesByUserId,
    user_id_under_test: UserId,
    now: Instant,
) {
    for _ in 0..2 {
        assert!(
            map.check_wait_and_record_password_attempt(user_id_under_test, || now)
                .is_ok()
        );
    }
    assert_eq!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || now),
        Err(Duration::from_secs(1))
    );
}

#[test]
fn parallel_owner_logins_with_attacker_wrong_attempts_keep_only_three_wrong_attempts_in_wait_count()
{
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map = WrongPasswordAttemptCountsAndLastAttemptTimesByUserId::default();
    let owner_request_b = map
        .check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
        .unwrap();
    let owner_request_a = map
        .check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
        .unwrap();
    for _ in 0..3 {
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
            .unwrap();
    }

    map.remove_wrong_sign_in_attempts_through_recorded_attempt(user_id_under_test, owner_request_b);
    map.remove_wrong_sign_in_attempts_through_recorded_attempt(user_id_under_test, owner_request_a);

    assert_exactly_two_more_attempts_pass_before_wait(
        &map,
        user_id_under_test,
        injected_time_start,
    );
}

#[test]
fn older_correct_password_does_not_clear_attempts_recorded_after_it() {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map =
        build_wrong_sign_in_attempts_map_for_user_at(user_id_under_test, 2, injected_time_start);
    let older_owner_request_a = map
        .check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
        .unwrap();
    let newer_owner_request_b = map
        .check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
        .unwrap();
    map.remove_wrong_sign_in_attempts_through_recorded_attempt(
        user_id_under_test,
        newer_owner_request_b,
    );
    let time_of_attacker_attempts = injected_time_start + Duration::from_millis(1);
    for _ in 0..3 {
        map.check_wait_and_record_password_attempt(user_id_under_test, || {
            time_of_attacker_attempts
        })
        .unwrap();
    }

    map.remove_wrong_sign_in_attempts_through_recorded_attempt(
        user_id_under_test,
        older_owner_request_a,
    );

    assert_exactly_two_more_attempts_pass_before_wait(
        &map,
        user_id_under_test,
        time_of_attacker_attempts,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_password_verification_future_holds_argon2_semaphore_permit_until_hash_work_ends()
{
    let hash = generate_hash("correct password");
    let password_hashing_semaphore_for_this_test = Arc::new(Semaphore::new(2));
    let free_argon2_password_hash_permits_before_verification =
        password_hashing_semaphore_for_this_test.available_permits();
    let mut verification = Box::pin(password_matches_account_hash_or_dummy_hash(
        wait_for_hash_slot_with_semaphore(Arc::clone(&password_hashing_semaphore_for_this_test))
            .await
            .unwrap(),
        "correct password".to_owned(),
        Some(hash),
    ));
    let password_verification_first_poll_was_pending = std::future::poll_fn(|context| {
        std::task::Poll::Ready(verification.as_mut().poll(context).is_pending())
    })
    .await;
    assert!(password_verification_first_poll_was_pending);

    drop(verification);

    assert_eq!(
        password_hashing_semaphore_for_this_test.available_permits(),
        free_argon2_password_hash_permits_before_verification - 1
    );
    let permit_wait_deadline = Instant::now() + Duration::from_secs(60);
    while password_hashing_semaphore_for_this_test.available_permits()
        != free_argon2_password_hash_permits_before_verification
    {
        assert!(
            Instant::now() < permit_wait_deadline,
            "permit never came back"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[test]
fn correct_password_clears_wrong_password_attempts_so_no_wait_remains() {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map =
        build_wrong_sign_in_attempts_map_for_user_at(user_id_under_test, 20, injected_time_start);
    assert_eq!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start),
        Err(Duration::from_secs(900))
    );

    let attempt_recorded_without_wait_check_for_live_session_same_account =
        map.record_wrong_password_attempt(user_id_under_test, || injected_time_start);
    map.remove_wrong_sign_in_attempts_through_recorded_attempt(
        user_id_under_test,
        attempt_recorded_without_wait_check_for_live_session_same_account,
    );

    assert!(
        map.check_wait_and_record_password_attempt(user_id_under_test, || injected_time_start)
            .is_ok()
    );
}

#[test]
fn removes_wrong_attempt_entries_older_than_24_hours_and_keeps_newer_ones_with_injected_instant() {
    let injected_time_start = Instant::now();
    let user_id_under_test = UserId::new(uuid::Uuid::now_v7());
    let map =
        build_wrong_sign_in_attempts_map_for_user_at(user_id_under_test, 6, injected_time_start);
    let day = Duration::from_millis(WRONG_PASSWORD_ENTRY_MAX_AGE_MILLISECONDS);

    map.remove_wrong_attempt_entries_older_than_24_hours(
        injected_time_start + day - Duration::from_secs(1),
    );
    assert_eq!(
        map.lock_sign_in_attempt_records()
            .wrong_password_attempt_records_by_user_id
            .len(),
        1
    );

    map.remove_wrong_attempt_entries_older_than_24_hours(injected_time_start + day);
    assert!(
        map.lock_sign_in_attempt_records()
            .wrong_password_attempt_records_by_user_id
            .is_empty()
    );
}

#[test]
fn client_network_matches_live_session_in_same_ipv4_address_or_ipv6_64_network_and_rejects_other_and_unparsable_stored_ip_strings()
 {
    let ip = |text: &str| text.parse::<IpAddr>().unwrap();

    assert!(client_network_matches_live_session_network(
        ip("2001:db8:1:2::99"),
        ["not an address", "2001:db8:1:2:aaaa::1"]
    ));
    assert!(client_network_matches_live_session_network(
        ip("::ffff:192.0.2.7"),
        ["192.0.2.7"]
    ));
    assert!(!client_network_matches_live_session_network(
        ip("2001:db8:1:3::99"),
        ["2001:db8:1:2::1"]
    ));
    assert!(!client_network_matches_live_session_network(
        ip("192.0.2.8"),
        ["192.0.2.7", "not an address"]
    ));
    assert!(!client_network_matches_live_session_network(
        ip("192.0.2.7"),
        []
    ));
}

static PASSWORD_HASHING_SEMAPHORE_ALL_SLOTS_TEST_MUTEX: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

async fn take_all_password_hash_semaphore_permits() -> OwnedSemaphorePermit {
    let held_semaphore_permits = Arc::clone(&ARGON2_PASSWORD_HASH_JOB_SEMAPHORE)
        .acquire_many_owned(
            u32::try_from(global_argon2_password_hashing_semaphore_permit_count()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ARGON2_PASSWORD_HASH_JOB_SEMAPHORE.available_permits(), 0);
    held_semaphore_permits
}

#[tokio::test(start_paused = true)]
async fn test_verify_password_returns_busy_error_when_all_hash_slots_stay_taken_until_wait_limit() {
    let password_verification_hash_slot_wait_start_instant = tokio::time::Instant::now();

    let verification = tokio::time::timeout(
        PASSWORD_HASH_SLOT_WAIT_TIMEOUT * 2,
        wait_for_hash_slot_with_semaphore(Arc::new(Semaphore::new(0))),
    )
    .await;

    assert!(
        matches!(
            verification,
            Ok(Err(WebError::PasswordHashBusy { retry_after })) if retry_after == PASSWORD_HASH_SLOT_WAIT_TIMEOUT
        ),
        "{verification:?}"
    );
    assert!(
        password_verification_hash_slot_wait_start_instant.elapsed()
            >= PASSWORD_HASH_SLOT_WAIT_TIMEOUT
    );
}

#[tokio::test(start_paused = true)]
async fn password_sign_in_through_router_returns_503_with_retry_after_while_all_password_hash_permits_are_held()
 {
    let _all_slots_test_guard = PASSWORD_HASHING_SEMAPHORE_ALL_SLOTS_TEST_MUTEX.lock().await;
    let held_semaphore_permits = take_all_password_hash_semaphore_permits().await;
    let account = gradient_fixtures::rows::user();
    let router_test_server_with_one_user = axum_test::TestServer::new(
        crate::create_router(
            gradient_test_support::state::TestState::new()
                .web_db(
                    gradient_fixtures::mock::mock_db()
                        .append_query_results([vec![account.clone()]])
                        .into_connection(),
                )
                .build(),
        )
        .unwrap()
        .layer(axum::extract::connect_info::MockConnectInfo(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )),
    );

    let password_sign_in_response_with_all_hash_slots_held = tokio::time::timeout(
        PASSWORD_HASH_SLOT_WAIT_TIMEOUT * 2,
        router_test_server_with_one_user
            .post("/api/v1/auth/basic/login")
            .json(
                &serde_json::json!({ "loginname": account.username, "password": "TestPass123!" }),
            ),
    )
    .await
    .unwrap();

    password_sign_in_response_with_all_hash_slots_held
        .assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        password_sign_in_response_with_all_hash_slots_held.header("retry-after"),
        "5"
    );
    drop(held_semaphore_permits);
}

#[tokio::test(start_paused = true)]
async fn six_sign_in_attempts_with_all_password_hash_slots_taken_return_503_then_free_slot_returns_401_not_429()
 {
    let _all_slots_test_guard = PASSWORD_HASHING_SEMAPHORE_ALL_SLOTS_TEST_MUTEX.lock().await;
    let held_semaphore_permits = take_all_password_hash_semaphore_permits().await;
    let account = gradient_fixtures::rows::user();
    let router_test_server_with_one_user = axum_test::TestServer::new(
        crate::create_router(
            gradient_test_support::state::TestState::new()
                .web_db(
                    gradient_fixtures::mock::mock_db()
                        .append_query_results(vec![vec![account.clone()]; 16])
                        .into_connection(),
                )
                .build(),
        )
        .unwrap()
        .layer(axum::extract::connect_info::MockConnectInfo(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        )),
    );
    let sign_in_request_body =
        serde_json::json!({ "loginname": account.username, "password": "WrongPass123!" });

    for i in 1..=6 {
        router_test_server_with_one_user
            .post("/api/v1/auth/basic/login")
            .add_header("x-forwarded-for", format!("198.51.100.{i}"))
            .json(&sign_in_request_body)
            .await
            .assert_status(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
    drop(held_semaphore_permits);

    router_test_server_with_one_user
        .post("/api/v1/auth/basic/login")
        .add_header("x-forwarded-for", "198.51.100.7")
        .json(&sign_in_request_body)
        .await
        .assert_status(axum::http::StatusCode::UNAUTHORIZED);
}
