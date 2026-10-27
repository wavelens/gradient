/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use gradient_wire::guest::RunJob;
use nix::sys::signal::{Signal, kill as send_signal};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::context::{UnknownPlaceholder, resolve_placeholders};
use crate::output::OutputLog;
use crate::setup::{PROGRAM_USER, PreparedJob};

const WORK_DIR: &str = "/work";
const SYSTEM_PATH: &str = "/run/current-system/sw/bin";
const PRE_SCRIPT: &str = "/etc/gradient/pre-script";
const POST_SCRIPT: &str = "/etc/gradient/post-script";
const KILL_GRACE: Duration = Duration::from_secs(10);
const SPAWN_FAILED: i32 = 127;

pub fn program_command(
    job: &RunJob,
    env: &BTreeMap<String, String>,
) -> Result<Command, UnknownPlaceholder> {
    let resolve = |text: &str| resolve_placeholders(text, &job.context, &job.secrets);
    let args = job
        .args
        .iter()
        .map(|arg| resolve(arg))
        .collect::<Result<Vec<_>, _>>()?;
    let app_env = job
        .env
        .iter()
        .map(|(name, value)| Ok((name.clone(), resolve(value)?)))
        .collect::<Result<BTreeMap<_, _>, UnknownPlaceholder>>()?;

    let groups = if job.groups.is_empty() {
        "--clear-groups".to_owned()
    } else {
        format!("--groups={}", job.groups.join(","))
    };
    let mut command = Command::new("setpriv");
    command
        .arg(format!("--reuid={PROGRAM_USER}"))
        .arg(format!("--regid={PROGRAM_USER}"))
        .arg(groups)
        .arg("--")
        .arg(&job.program)
        .args(args)
        .current_dir(WORK_DIR)
        .env_clear()
        .env("PATH", SYSTEM_PATH)
        .env("HOME", format!("/home/{PROGRAM_USER}"))
        .envs(app_env)
        .envs(env);
    Ok(command)
}

pub async fn run(
    prepared: PreparedJob,
    output: &mut OutputLog,
    started: oneshot::Sender<()>,
    kill: CancellationToken,
) -> i32 {
    let PreparedJob { program, env } = prepared;
    let code = match run_script(PRE_SCRIPT, &env, output, &kill).await {
        0 if !kill.is_cancelled() => run_program(program, output, started, &kill).await,
        0 => SPAWN_FAILED,
        code => {
            note(output, &format!("pre-script exited with code {code}")).await;
            code
        }
    };

    let post = run_script(POST_SCRIPT, &env, output, &CancellationToken::new()).await;
    if post != 0 {
        note(output, &format!("post-script exited with code {post}")).await;
    }
    code
}

async fn run_program(
    mut program: Command,
    output: &mut OutputLog,
    started: oneshot::Sender<()>,
    kill: &CancellationToken,
) -> i32 {
    match spawn_piped(&mut program) {
        Ok(child) => {
            let _ = started.send(());
            exit_code(stream_until_exit(child, output, kill).await)
        }
        Err(e) => {
            note(output, &format!("cannot start the program: {e}")).await;
            SPAWN_FAILED
        }
    }
}

async fn run_script(
    path: &str,
    env: &BTreeMap<String, String>,
    output: &mut OutputLog,
    kill: &CancellationToken,
) -> i32 {
    if !Path::new(path).exists() {
        return 0;
    }

    let mut command = Command::new(path);
    command
        .current_dir(WORK_DIR)
        .env_clear()
        .env("PATH", SYSTEM_PATH)
        .env("HOME", "/root")
        .envs(env);
    match spawn_piped(&mut command) {
        Ok(child) => exit_code(stream_until_exit(child, output, kill).await),
        Err(e) => {
            note(output, &format!("cannot start {path}: {e}")).await;
            SPAWN_FAILED
        }
    }
}

fn spawn_piped(command: &mut Command) -> std::io::Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

async fn stream_until_exit(
    mut child: Child,
    output: &mut OutputLog,
    kill: &CancellationToken,
) -> std::io::Result<ExitStatus> {
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let exit = async {
        pump(stdout, stderr, output).await;
        child.wait().await
    };
    tokio::pin!(exit);

    tokio::select! {
        status = &mut exit => return status,
        _ = kill.cancelled() => signal(pid, Signal::SIGTERM),
    }
    tokio::select! {
        status = &mut exit => status,
        _ = tokio::time::sleep(KILL_GRACE) => {
            signal(pid, Signal::SIGKILL);
            exit.await
        }
    }
}

async fn pump(
    stdout: Option<impl AsyncRead + Unpin>,
    stderr: Option<impl AsyncRead + Unpin>,
    output: &mut OutputLog,
) {
    let (mut stdout, mut stderr) = (stdout, stderr);
    let mut out_buf = vec![0u8; 8192];
    let mut err_buf = vec![0u8; 8192];
    while stdout.is_some() || stderr.is_some() {
        tokio::select! {
            read = read_some(stdout.as_mut(), &mut out_buf) => keep(read, &out_buf, &mut stdout, output).await,
            read = read_some(stderr.as_mut(), &mut err_buf) => keep(read, &err_buf, &mut stderr, output).await,
        }
    }
}

async fn keep<P>(read: Option<usize>, buf: &[u8], pipe: &mut Option<P>, output: &mut OutputLog) {
    match read {
        Some(n) => {
            if let Err(e) = output.append(&buf[..n]).await {
                tracing::warn!(error = %e, "cannot keep program output");
            }
        }
        None => *pipe = None,
    }
}

async fn read_some(pipe: Option<&mut (impl AsyncRead + Unpin)>, buf: &mut [u8]) -> Option<usize> {
    match pipe {
        Some(pipe) => match pipe.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        None => std::future::pending().await,
    }
}

fn signal(pid: Option<Pid>, signal: Signal) {
    if let Some(pid) = pid {
        let _ = send_signal(pid, signal);
    }
}

fn exit_code(status: std::io::Result<ExitStatus>) -> i32 {
    match status {
        Ok(status) => status
            .code()
            .or_else(|| status.signal().map(|n| 128 + n))
            .unwrap_or(SPAWN_FAILED),
        Err(_) => SPAWN_FAILED,
    }
}

async fn note(output: &mut OutputLog, line: &str) {
    if let Err(e) = output.append(format!("{line}\n").as_bytes()).await {
        tracing::warn!(error = %e, "cannot keep program output");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use gradient_wire::guest::{ContextLayers, RunJob};

    use super::program_command;

    fn job() -> RunJob {
        RunJob {
            program: "/nix/store/abc-train/bin/train".to_owned(),
            args: vec!["--commit=@gradient.job.commit@".to_owned()],
            env: BTreeMap::from([("GRADIENT_JOB".to_owned(), "x".to_owned())]),
            context: ContextLayers {
                job: BTreeMap::from([
                    ("commit".to_owned(), "abc".to_owned()),
                    ("job".to_owned(), "j1".to_owned()),
                ]),
                ..ContextLayers::default()
            },
            groups: vec!["kvm".to_owned(), "dialout".to_owned()],
            ..RunJob::default()
        }
    }

    #[test]
    fn the_program_runs_as_gradient_with_the_device_groups() {
        let command = program_command(&job(), &BTreeMap::new()).expect("command");
        let command = command.as_std();
        let argv = std::iter::once(command.get_program())
            .chain(command.get_args())
            .collect::<Vec<_>>();

        assert_eq!(
            argv,
            [
                "setpriv",
                "--reuid=gradient",
                "--regid=gradient",
                "--groups=kvm,dialout",
                "--",
                "/nix/store/abc-train/bin/train",
                "--commit=abc",
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn gradient_variables_win_over_app_env() {
        let env = BTreeMap::from([("GRADIENT_JOB".to_owned(), "j1".to_owned())]);
        let command = program_command(&job(), &env).expect("command");
        let job_var = command
            .as_std()
            .get_envs()
            .find(|(name, _)| *name == "GRADIENT_JOB")
            .and_then(|(_, value)| value);

        assert_eq!(job_var, Some(OsStr::new("j1")));
    }
}
