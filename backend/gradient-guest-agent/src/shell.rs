/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::io;
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::unix::pipe;
use tokio::process::{Child, Command};

use crate::run::{SYSTEM_PATH, WORK_DIR, as_program_user};
use crate::terminal::{DEFAULT_COLS, DEFAULT_ROWS, PtyMaster, TERMINAL_TYPE, spawn_on_pty};

#[derive(Clone, Debug, Default)]
pub struct ShellContext {
    pub env: BTreeMap<String, String>,
    pub groups: Vec<String>,
}

pub struct Shell {
    pub child: Child,
    pub input: Box<dyn AsyncWrite + Send + Unpin>,
    pub output: Box<dyn AsyncRead + Send + Unpin>,
    pub terminal: Option<Arc<PtyMaster>>,
}

pub fn shell_command(context: &ShellContext, pty: bool) -> Command {
    let mut command = as_program_user(&context.groups, pty);
    command
        .arg(format!("{SYSTEM_PATH}/bash"))
        .current_dir(WORK_DIR)
        .env_clear()
        .envs(&context.env);
    if pty {
        command.env("TERM", TERMINAL_TYPE);
    }
    command
}

pub fn spawn_shell(context: &ShellContext, pty: bool) -> io::Result<Shell> {
    let command = shell_command(context, pty);
    if pty {
        spawn_on_terminal(command)
    } else {
        spawn_with_pipes(command)
    }
}

fn spawn_on_terminal(command: Command) -> io::Result<Shell> {
    let (child, master) = spawn_on_pty(command, DEFAULT_COLS, DEFAULT_ROWS)?;
    Ok(Shell {
        child,
        input: Box::new(master.stream()?),
        output: Box::new(master.stream()?),
        terminal: Some(master),
    })
}

fn spawn_with_pipes(mut command: Command) -> io::Result<Shell> {
    let (output, output_writer) = std::io::pipe()?;
    command
        .stdin(Stdio::piped())
        .stdout(output_writer.try_clone()?)
        .stderr(output_writer)
        .process_group(0);
    let mut child = command.spawn()?;
    drop(command);
    let input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("the shell has no stdin"))?;
    Ok(Shell {
        child,
        input: Box::new(input),
        output: Box::new(pipe::Receiver::from_owned_fd(OwnedFd::from(output))?),
        terminal: None,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn context() -> ShellContext {
        ShellContext {
            env: BTreeMap::from([("GRADIENT_JOB".to_owned(), "j1".to_owned())]),
            groups: vec!["kvm".to_owned()],
        }
    }

    fn env_of<'a>(command: &'a Command, name: &str) -> Option<&'a OsStr> {
        command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value)
    }

    fn argv(command: &Command) -> Vec<&OsStr> {
        let std = command.as_std();
        std::iter::once(std.get_program())
            .chain(std.get_args())
            .collect()
    }

    #[test]
    fn a_shell_runs_as_gradient_in_work_with_the_job_env() {
        let as_gradient = [
            "setpriv",
            "--reuid=gradient",
            "--regid=gradient",
            "--groups=kvm",
            "--",
            "/run/current-system/sw/bin/bash",
        ]
        .map(OsStr::new);
        let with_pipes = shell_command(&context(), false);
        let with_terminal = shell_command(&context(), true);

        assert_eq!(argv(&with_pipes), as_gradient);
        assert_eq!(
            argv(&with_terminal)[..2],
            ["setsid", "--ctty"].map(OsStr::new)
        );
        assert_eq!(argv(&with_terminal)[2..], as_gradient);
        assert_eq!(
            with_terminal.as_std().get_current_dir(),
            Some(std::path::Path::new("/work"))
        );
        assert_eq!(
            env_of(&with_terminal, "GRADIENT_JOB"),
            Some(OsStr::new("j1"))
        );
        assert_eq!(
            env_of(&with_terminal, "TERM"),
            Some(OsStr::new("xterm-256color"))
        );
        assert_eq!(env_of(&with_pipes, "TERM"), None);
    }
}
