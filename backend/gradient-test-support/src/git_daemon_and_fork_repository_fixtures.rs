/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct GitDaemonProcessGuard(Child);

impl Drop for GitDaemonProcessGuard {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

pub fn start_git_daemon_on_free_loopback_port_serving_base_directory(
    base: &Path,
) -> (GitDaemonProcessGuard, String) {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let exec_path = Command::new("git")
        .arg("--exec-path")
        .output()
        .unwrap()
        .stdout;
    let exec_path = PathBuf::from(String::from_utf8(exec_path).unwrap().trim());
    let daemon = Command::new(exec_path.join("git-daemon"))
        .args(["--export-all", "--reuseaddr", "--listen=127.0.0.1"])
        .arg(format!("--port={port}"))
        .arg(format!("--base-path={}", base.display()))
        .spawn()
        .unwrap();
    let daemon = GitDaemonProcessGuard(daemon);
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "git daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    (daemon, format!("git://127.0.0.1:{port}"))
}

fn run_git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args([
            "-c",
            "user.name=Test User",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn write_file_and_commit(repository: &Path, file: &str) -> String {
    let path = repository.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, file).unwrap();
    run_git(repository, &["add", "-A"]);
    run_git(repository, &["commit", "-q", "-m", file]);
    run_git(repository, &["rev-parse", "HEAD"])
}

pub fn create_forked_repository_with_base_commit_and_fork_commit_and_return_fork_head_commit_hash(
    served: &Path,
    base_file: &str,
    fork_file: &str,
) -> String {
    let base = served.join("test-org/repo");
    let fork = served.join("external/repo");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::create_dir_all(fork.parent().unwrap()).unwrap();
    run_git(&base, &["init", "-q", "-b", "main"]);
    write_file_and_commit(&base, base_file);
    let (base_path, fork_path) = (base.to_str().unwrap(), fork.to_str().unwrap());
    run_git(served, &["clone", "-q", base_path, fork_path]);
    write_file_and_commit(&fork, fork_file)
}
