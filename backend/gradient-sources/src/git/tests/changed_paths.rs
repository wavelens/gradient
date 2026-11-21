/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::fixtures::run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout;
use crate::git::changed_paths::{
    ChangedFileComparison, ChangedFileComparisonCacheWithOneRunPerProjectAndRepository,
    ChangedFilePathsAndBaseCommit, ComparisonBaseCommit,
    changed_file_list_has_at_most_max_paths_and_can_be_kept_in_comparison_cache,
    changed_files_cache_duration_for_comparison_base,
    fetch_base_and_head_into_temp_repo_and_get_changed_paths,
    project_git_credentials_for_pull_request_head_on_same_host_and_port,
};
use crate::git::git_credentials::{GitOperationCredentials, HttpsGitCredential};
use crate::git::guarded_git_network_connections::set_guarded_git_client_to_allow_loopback_for_tests;
use crate::git::ssh::SshGitPrivateKeyKnownHostsAndObservations;
use crate::git::ssh::tests::{
    decode, ed25519_pem, generate_random_ssh_host_private_key, serve_git_over_ssh,
};
use gradient_types::RepositoryUrl;
use gradient_types::ids::ProjectId;
use gradient_util::shutdown::Shutdown;
use russh::keys::Algorithm;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn create_temporary_git_repository_on_main_with_initial_commit_of_files(
    files: &[&str],
) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("r");
    std::fs::create_dir(&repo).unwrap();
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        &repo,
        &["init", "-b", "main"],
    );
    for file in files {
        std::fs::write(repo.join(file), file).unwrap();
    }
    stage_all_changes_commit_and_get_commit_hash_bytes(&repo);
    (tmp, repo)
}

fn stage_all_changes_commit_and_get_commit_hash_bytes(repo: &Path) -> Vec<u8> {
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(repo, &["add", "-A"]);
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        repo,
        &["commit", "-m", "change"],
    );
    hex::decode(
        run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
            repo,
            &["rev-parse", "HEAD"],
        ),
    )
    .unwrap()
}

fn url(repo: &Path) -> String {
    format!("file://{}", repo.display())
}

fn changed(
    base_repo: &Path,
    base: &ComparisonBaseCommit,
    head_repo: &Path,
    head: &[u8],
) -> Result<ChangedFilePathsAndBaseCommit, crate::SourceError> {
    fetch_base_and_head_into_temp_repo_and_get_changed_paths(
        &ChangedFileComparison {
            task_repository_url: RepositoryUrl::local_test_repository(base_repo),
            base: base.clone(),
            head_commit_repository_url: RepositoryUrl::local_test_repository(head_repo),
            head: head.to_vec(),
        },
        &GitOperationCredentials::default(),
    )
}

type SharedComparisonCacheWithStringKeysU32ComparisonsU32OutputsUnitErrors =
    Arc<ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<&'static str, u32, u32, ()>>;

fn make_run_closure_that_counts_sleeps_and_returns_output(
    runs: &Arc<AtomicUsize>,
    output: u32,
    duration: Duration,
) -> impl FnOnce() -> futures::future::BoxFuture<'static, Result<u32, ()>> {
    let runs = Arc::clone(runs);
    move || {
        runs.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            tokio::time::sleep(duration).await;
            Ok(output)
        })
    }
}

#[tokio::test]
async fn two_concurrent_callers_of_same_changed_file_comparison_cause_one_run_and_get_its_result() {
    let cache = SharedComparisonCacheWithStringKeysU32ComparisonsU32OutputsUnitErrors::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();

    let (first, second) = tokio::join!(
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                10,
                Duration::from_millis(100)
            ),
        ),
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                20,
                Duration::from_millis(100)
            ),
        ),
    );

    assert_eq!((first, second), (Some(Ok(10)), Some(Ok(10))));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn two_concurrent_callers_of_same_failing_changed_file_comparison_get_error_from_one_run() {
    let cache = SharedComparisonCacheWithStringKeysU32ComparisonsU32OutputsUnitErrors::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();
    let failing_run = || {
        let runs = Arc::clone(&runs);
        move || {
            runs.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Err::<u32, ()>(())
            }
        }
    };

    let (first, second) = tokio::join!(
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            failing_run()
        ),
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            failing_run()
        ),
    );

    assert_eq!((first, second), (Some(Err(())), Some(Err(()))));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn caller_waits_for_running_comparison_on_same_key_to_end() {
    let cache = SharedComparisonCacheWithStringKeysU32ComparisonsU32OutputsUnitErrors::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();
    let started = Instant::now();

    let (first, second) = tokio::join!(
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                10,
                Duration::from_millis(200)
            ),
        ),
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let second = cache
                .get_or_wait_for_or_start_comparison_result(
                    &shutdown_task_tracker,
                    "repository",
                    2,
                    make_run_closure_that_counts_sleeps_and_returns_output(
                        &runs,
                        20,
                        Duration::from_millis(200),
                    ),
                )
                .await;
            (second, started.elapsed())
        },
    );

    assert_eq!(first, Some(Ok(10)));
    assert_eq!(second.0, Some(Ok(20)));
    assert!(second.1 >= Duration::from_millis(400), "{:?}", second.1);
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn caller_whose_time_budget_ends_waiting_for_same_key_comparison_times_out_and_never_starts_run()
 {
    let cache = SharedComparisonCacheWithStringKeysU32ComparisonsU32OutputsUnitErrors::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();

    let (first, second) = tokio::join!(
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                10,
                Duration::from_millis(300)
            ),
        ),
        async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tokio::time::timeout(
                Duration::from_millis(50),
                cache.get_or_wait_for_or_start_comparison_result(
                    &shutdown_task_tracker,
                    "repository",
                    2,
                    make_run_closure_that_counts_sleeps_and_returns_output(
                        &runs,
                        20,
                        Duration::ZERO,
                    ),
                ),
            )
            .await
        },
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(first, Some(Ok(10)));
    assert!(second.is_err(), "{second:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stored_finished_comparison_result_answers_later_caller_without_rerun() {
    let cache = SharedComparisonCacheWithStringKeysU32ComparisonsU32OutputsUnitErrors::default();
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();

    let first = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 10, Duration::ZERO),
        )
        .await;
    let later = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            1,
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 20, Duration::ZERO),
        )
        .await;

    assert_eq!((first, later), (Some(Ok(10)), Some(Ok(10))));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

fn build_shared_changed_file_comparison_cache_with_production_storage_time() -> Arc<
    ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<
        &'static str,
        (ProjectId, ChangedFileComparison),
        u32,
        (),
    >,
> {
    Arc::new(
        ChangedFileComparisonCacheWithOneRunPerProjectAndRepository::new(
            changed_files_cache_duration_for_comparison_base,
            |_| true,
        ),
    )
}

async fn count_comparison_runs_for_two_cache_calls_with_changed_file_list_length(
    path_count: usize,
) -> usize {
    let cache: Arc<
        ChangedFileComparisonCacheWithOneRunPerProjectAndRepository<
            &'static str,
            (ProjectId, ChangedFileComparison),
            ChangedFilePathsAndBaseCommit,
            (),
        >,
    > = Arc::new(
        ChangedFileComparisonCacheWithOneRunPerProjectAndRepository::new(
            changed_files_cache_duration_for_comparison_base,
            changed_file_list_has_at_most_max_paths_and_can_be_kept_in_comparison_cache,
        ),
    );
    let comparison =
        build_project_id_and_changed_file_comparison_with_head_byte(ProjectId::now_v7(), 0xab);
    let list = ChangedFilePathsAndBaseCommit {
        changed_file_paths: (0..path_count).map(|i| format!("src/{i}")).collect(),
        comparison_base_commit_id_bytes: vec![0; 20],
    };
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();
    for _ in 0..2 {
        let (run_counter, run_output) = (Arc::clone(&runs), list.clone());
        let output = cache
            .get_or_wait_for_or_start_comparison_result(
                &shutdown_task_tracker,
                "repository",
                comparison.clone(),
                move || {
                    run_counter.fetch_add(1, Ordering::SeqCst);
                    async move { Ok(run_output) }
                },
            )
            .await;
        assert_eq!(output, Some(Ok(list.clone())));
    }
    runs.load(Ordering::SeqCst)
}

#[tokio::test]
async fn changed_file_list_over_limit_is_returned_without_caching_and_second_call_recomputes() {
    assert_eq!(
        count_comparison_runs_for_two_cache_calls_with_changed_file_list_length(10_000).await,
        1
    );
    assert_eq!(
        count_comparison_runs_for_two_cache_calls_with_changed_file_list_length(10_001).await,
        2
    );
}

fn build_project_id_and_changed_file_comparison_with_head_byte(
    project: ProjectId,
    head_byte: u8,
) -> (ProjectId, ChangedFileComparison) {
    let task_and_head_comparison_repository_url =
        RepositoryUrl::local_test_repository(Path::new("/repository"));
    (
        project,
        ChangedFileComparison {
            task_repository_url: task_and_head_comparison_repository_url.clone(),
            base: ComparisonBaseCommit::BranchName("main".into()),
            head_commit_repository_url: task_and_head_comparison_repository_url,
            head: vec![head_byte; 20],
        },
    )
}

#[tokio::test(start_paused = true)]
async fn comparison_finishes_after_only_caller_timeout_and_next_caller_with_same_branch_name_and_head_gets_stored_list()
 {
    let cache = build_shared_changed_file_comparison_cache_with_production_storage_time();
    let comparison =
        build_project_id_and_changed_file_comparison_with_head_byte(ProjectId::now_v7(), 0xab);
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();

    let timed_out = tokio::time::timeout(
        Duration::from_secs(4),
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            comparison.clone(),
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                10,
                Duration::from_secs(30),
            ),
        ),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(60)).await;
    let later = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            comparison,
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 20, Duration::ZERO),
        )
        .await;

    assert!(timed_out.is_err(), "{timed_out:?}");
    assert_eq!(later, Some(Ok(10)));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn stored_branch_comparison_with_same_branch_name_and_different_head_commit_starts_new_run() {
    let cache = build_shared_changed_file_comparison_cache_with_production_storage_time();
    let project = ProjectId::now_v7();
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();

    let first = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            build_project_id_and_changed_file_comparison_with_head_byte(project, 0xab),
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 10, Duration::ZERO),
        )
        .await;
    let other_head = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            build_project_id_and_changed_file_comparison_with_head_byte(project, 0xcd),
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 20, Duration::ZERO),
        )
        .await;

    assert_eq!((first, other_head), (Some(Ok(10)), Some(Ok(20))));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn cached_branch_base_comparison_expires_and_next_caller_starts_new_run() {
    let cache = build_shared_changed_file_comparison_cache_with_production_storage_time();
    let comparison =
        build_project_id_and_changed_file_comparison_with_head_byte(ProjectId::now_v7(), 0xab);
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();

    let first = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            comparison.clone(),
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 10, Duration::ZERO),
        )
        .await;
    tokio::time::sleep(Duration::from_secs(2 * 60 + 1)).await;
    let after_storage_time = cache
        .get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            "repository",
            comparison,
            make_run_closure_that_counts_sleeps_and_returns_output(&runs, 20, Duration::ZERO),
        )
        .await;

    assert_eq!((first, after_storage_time), (Some(Ok(10)), Some(Ok(20))));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn same_repository_url_in_two_projects_gives_two_separate_runs() {
    let cache = Arc::new(
        ChangedFileComparisonCacheWithOneRunPerProjectAndRepository::<
            (ProjectId, &'static str),
            (ProjectId, u32),
            u32,
            (),
        >::default(),
    );
    let runs = Arc::new(AtomicUsize::new(0));
    let shutdown_task_tracker = Shutdown::new();
    let (first_project, second_project) = (ProjectId::now_v7(), ProjectId::now_v7());

    let (first, second) = tokio::join!(
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            (first_project, "repository"),
            (first_project, 1),
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                10,
                Duration::from_millis(100)
            ),
        ),
        cache.get_or_wait_for_or_start_comparison_result(
            &shutdown_task_tracker,
            (second_project, "repository"),
            (second_project, 1),
            make_run_closure_that_counts_sleeps_and_returns_output(
                &runs,
                20,
                Duration::from_millis(100)
            ),
        ),
    );

    assert_eq!((first, second), (Some(Ok(10)), Some(Ok(20))));
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

#[test]
fn pull_request_head_fetch_from_fork_url_gets_no_ssh_key_or_https_login_on_other_host_or_port_but_same_host_and_port_keeps_ssh_key_and_https_login()
 {
    let project_repository: RepositoryUrl = "git://127.0.0.1:9418/test-org/repo".parse().unwrap();
    let fork_on_same_server: RepositoryUrl = "git://127.0.0.1:9418/external/repo".parse().unwrap();
    let fork_on_other_port: RepositoryUrl = "git://127.0.0.1:9419/external/repo".parse().unwrap();
    let fork_on_other_host: RepositoryUrl = "git://127.0.0.2:9418/external/repo".parse().unwrap();
    let credentials = GitOperationCredentials::from_ssh_key_known_hosts_and_https_credentials(
        Some(SshGitPrivateKeyKnownHostsAndObservations::new(
            ed25519_pem().into(),
            Vec::new(),
        )),
        vec![HttpsGitCredential {
            host: "127.0.0.1".to_owned(),
            username: "ci".to_owned(),
            password: "token".into(),
        }],
    );

    let same_server = project_git_credentials_for_pull_request_head_on_same_host_and_port(
        &project_repository,
        &fork_on_same_server,
        &credentials,
    );

    for fork in [&fork_on_other_port, &fork_on_other_host] {
        let other_server = project_git_credentials_for_pull_request_head_on_same_host_and_port(
            &project_repository,
            fork,
            &credentials,
        );
        assert!(
            other_server.ssh_git_private_key_and_known_hosts.is_none(),
            "{fork:?}"
        );
        assert!(other_server.https_git_credentials.is_empty(), "{fork:?}");
    }
    assert!(same_server.ssh_git_private_key_and_known_hosts.is_some());
    assert_eq!(same_server.https_git_credentials.len(), 1);
}

#[test]
fn changed_paths_returns_added_modified_deleted_and_renamed_old_and_new_paths_between_commits() {
    let (_tmp, repo) = create_temporary_git_repository_on_main_with_initial_commit_of_files(&[
        "kept", "modified", "deleted", "renamed",
    ]);
    let base = hex::decode(
        run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
            &repo,
            &["rev-parse", "HEAD"],
        ),
    )
    .unwrap();
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/added"), "added").unwrap();
    std::fs::write(repo.join("modified"), "changed contents").unwrap();
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        &repo,
        &["rm", "-q", "deleted"],
    );
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        &repo,
        &["mv", "renamed", "moved"],
    );
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&repo);

    let paths = changed(
        &repo,
        &ComparisonBaseCommit::CommitHashBytes(base),
        &repo,
        &head,
    )
    .unwrap();

    assert_eq!(
        paths.changed_file_paths,
        ["deleted", "modified", "moved", "renamed", "src/added"]
    );
}

#[test]
fn changed_paths_returns_error_when_base_commit_does_not_exist() {
    let (_tmp, repo) = create_temporary_git_repository_on_main_with_initial_commit_of_files(&["f"]);
    std::fs::write(repo.join("f"), "changed").unwrap();
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&repo);

    let result = changed(
        &repo,
        &ComparisonBaseCommit::CommitHashBytes(vec![0xab; 20]),
        &repo,
        &head,
    );

    assert!(result.is_err(), "{result:?}");
}

#[test]
fn changed_paths_with_main_branch_base_and_fork_head_returns_changed_file() {
    let (_upstream_tmp, upstream) =
        create_temporary_git_repository_on_main_with_initial_commit_of_files(&["docs", "src"]);
    let fork_tmp = tempfile::tempdir().unwrap();
    let fork = fork_tmp.path().join("fork");
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        fork_tmp.path(),
        &["clone", "-q", &url(&upstream), fork.to_str().unwrap()],
    );
    std::fs::write(fork.join("src"), "changed in the fork").unwrap();
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&fork);

    let main_tip = hex::decode(
        run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
            &upstream,
            &["rev-parse", "main"],
        ),
    )
    .unwrap();

    let changed_file_paths_and_base_commit = changed(
        &upstream,
        &ComparisonBaseCommit::BranchName("main".into()),
        &fork,
        &head,
    )
    .unwrap();

    assert_eq!(
        changed_file_paths_and_base_commit,
        ChangedFilePathsAndBaseCommit {
            changed_file_paths: vec!["src".to_owned()],
            comparison_base_commit_id_bytes: main_tip,
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_changed_paths_between_ssh_project_repository_and_fork_pull_request_head_on_another_port_fails_with_only_project_credentials()
 {
    let (_upstream_tmp, upstream) =
        create_temporary_git_repository_on_main_with_initial_commit_of_files(&["docs", "src"]);
    let fork_tmp = tempfile::tempdir().unwrap();
    let fork = fork_tmp.path().join("fork");
    run_git_command_in_repository_with_fixed_author_and_return_trimmed_stdout(
        fork_tmp.path(),
        &["clone", "-q", &url(&upstream), fork.to_str().unwrap()],
    );
    std::fs::write(fork.join("src"), "changed in the fork").unwrap();
    let head = stage_all_changes_commit_and_get_commit_hash_bytes(&fork);
    let client_pem = ed25519_pem();
    let mut project_and_fork_ssh_server_ports = Vec::new();
    for _ in 0..2 {
        project_and_fork_ssh_server_ports.push(
            serve_git_over_ssh(
                decode(&client_pem).public_key().clone(),
                vec![generate_random_ssh_host_private_key(Algorithm::Ed25519)],
            )
            .await,
        );
    }
    let ssh_repository_url_for_localhost_port_and_directory =
        |port: u16, repository: &Path| -> RepositoryUrl {
            format!("ssh://git@127.0.0.1:{port}{}", repository.display())
                .parse()
                .unwrap()
        };
    let build_changed_file_comparison_with_fork_head_on_port = |port: u16| ChangedFileComparison {
        task_repository_url: ssh_repository_url_for_localhost_port_and_directory(
            project_and_fork_ssh_server_ports[0],
            &upstream,
        ),
        base: ComparisonBaseCommit::BranchName("main".into()),
        head_commit_repository_url: ssh_repository_url_for_localhost_port_and_directory(
            port, &fork,
        ),
        head: head.clone(),
    };
    let fork_head_on_same_ssh_server_and_port_as_project =
        build_changed_file_comparison_with_fork_head_on_port(project_and_fork_ssh_server_ports[0]);
    let fork_head_and_project_repository_ssh_server_ports_differ =
        build_changed_file_comparison_with_fork_head_on_port(project_and_fork_ssh_server_ports[1]);
    let credentials = GitOperationCredentials::from(Some(
        SshGitPrivateKeyKnownHostsAndObservations::new(client_pem.into(), Vec::new()),
    ));

    let (changed_paths_result_for_fork_on_same_port, changed_paths_for_fork_on_other_port_result) =
        tokio::task::spawn_blocking(move || {
            (
                fetch_base_and_head_into_temp_repo_and_get_changed_paths(
                    &fork_head_on_same_ssh_server_and_port_as_project,
                    &credentials,
                ),
                fetch_base_and_head_into_temp_repo_and_get_changed_paths(
                    &fork_head_and_project_repository_ssh_server_ports_differ,
                    &credentials,
                ),
            )
        })
        .await
        .unwrap();

    assert_eq!(
        changed_paths_result_for_fork_on_same_port
            .unwrap()
            .changed_file_paths,
        ["src"]
    );
    assert!(
        changed_paths_for_fork_on_other_port_result.is_err(),
        "{changed_paths_for_fork_on_other_port_result:?}"
    );
}

fn spawn_tcp_listener_that_accepts_and_holds_connection_open()
-> (u16, mpsc::Receiver<()>, mpsc::Sender<()>) {
    set_guarded_git_client_to_allow_loopback_for_tests();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (fetch_connection_accepted_sender, fetch_connection_accepted_receiver) = mpsc::channel();
    let (close_accepted_connection_sender, close_accepted_connection_receiver) =
        mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (connection, _) = listener.accept().unwrap();
        fetch_connection_accepted_sender.send(()).unwrap();
        let _ = close_accepted_connection_receiver.recv();
        drop(connection);
    });
    (
        port,
        fetch_connection_accepted_receiver,
        close_accepted_connection_sender,
    )
}

#[tokio::test]
async fn cancel_and_drain_returns_true_while_git_fetch_is_stalled_on_unresponsive_listener() {
    let (port, fetch_connection_accepted_receiver, close_accepted_connection_sender) =
        spawn_tcp_listener_that_accepts_and_holds_connection_open();
    let repository_url: RepositoryUrl = format!("git://127.0.0.1:{port}/r.git").parse().unwrap();
    let comparison = ChangedFileComparison {
        task_repository_url: repository_url.clone(),
        base: ComparisonBaseCommit::BranchName("main".into()),
        head_commit_repository_url: repository_url,
        head: vec![1; 20],
    };
    let cache = Arc::new(
        ChangedFileComparisonCacheWithOneRunPerProjectAndRepository::<
            (),
            ChangedFileComparison,
            ChangedFilePathsAndBaseCommit,
            crate::SourceError,
        >::default(),
    );
    let shutdown = Shutdown::new();
    let blocking_comparison = comparison.clone();
    let caller = cache.get_or_wait_for_or_start_comparison_result(
        &shutdown,
        (),
        comparison,
        move || async move {
            tokio::task::spawn_blocking(move || {
                fetch_base_and_head_into_temp_repo_and_get_changed_paths(
                    &blocking_comparison,
                    &GitOperationCredentials::default(),
                )
            })
            .await
            .unwrap()
        },
    );
    let _ = tokio::time::timeout(Duration::from_millis(100), caller).await;
    tokio::task::spawn_blocking(move || {
        fetch_connection_accepted_receiver.recv_timeout(Duration::from_secs(5))
    })
    .await
    .unwrap()
    .unwrap();

    let all_background_tasks_ended = shutdown.cancel_and_drain(Duration::from_secs(2)).await;
    close_accepted_connection_sender.send(()).unwrap();

    assert!(
        all_background_tasks_ended,
        "Shutdown drain waited for a git fetch on a server that did not respond."
    );
}
