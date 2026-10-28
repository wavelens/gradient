/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::ExecJobId;
use gradient_wire::types::GuestChannelKind;
use uuid::Uuid;

pub const ONLY_SHELL: &str = "only the command shell is supported in an exec job login";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayLogin {
    pub project: String,
    pub target: Option<GatewayTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayTarget {
    Job { job: ExecJobId, node: u32 },
    Task(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the login is not <project>, <project>/<job>, <project>/<job>/<node> or <project>/<task>")]
pub struct BadLogin;

pub fn parse_login(user: &str) -> Result<GatewayLogin, BadLogin> {
    let segments: Vec<&str> = user.split('/').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err(BadLogin);
    }
    let target = match segments[1..] {
        [] => None,
        [target] => Some(job_or_task(target)),
        [job, node] => Some(job_on_node(job, node)?),
        _ => return Err(BadLogin),
    };
    Ok(GatewayLogin {
        project: segments[0].to_owned(),
        target,
    })
}

fn job_or_task(segment: &str) -> GatewayTarget {
    match segment.parse::<Uuid>() {
        Ok(id) => GatewayTarget::Job {
            job: ExecJobId::new(id),
            node: 0,
        },
        Err(_) => GatewayTarget::Task(segment.to_owned()),
    }
}

fn job_on_node(job: &str, node: &str) -> Result<GatewayTarget, BadLogin> {
    let job = job.parse::<Uuid>().map_err(|_| BadLogin)?;
    let node = node.parse::<u32>().map_err(|_| BadLogin)?;
    Ok(GatewayTarget::Job {
        job: ExecJobId::new(job),
        node,
    })
}

pub fn gateway_shell_kind(
    command: Option<&[u8]>,
    pty: bool,
) -> Result<GuestChannelKind, &'static str> {
    match command {
        None => Ok(GuestChannelKind::Shell { pty, attach: true }),
        Some(b"shell") => Ok(GuestChannelKind::Shell { pty, attach: false }),
        Some(_) => Err(ONLY_SHELL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JOB: &str = "0192f0a4-1c2b-7d3e-8f40-5a6b7c8d9e0f";

    fn job() -> ExecJobId {
        ExecJobId::new(JOB.parse().unwrap())
    }

    fn target_of(user: &str) -> Option<GatewayTarget> {
        parse_login(user).unwrap().target
    }

    #[test]
    fn logins_name_a_project_a_job_a_node_or_a_task() {
        assert_eq!(
            parse_login("infra"),
            Ok(GatewayLogin {
                project: "infra".into(),
                target: None
            })
        );
        let on_first_node = Some(GatewayTarget::Job {
            job: job(),
            node: 0,
        });
        assert_eq!(target_of(&format!("infra/{JOB}")), on_first_node);
        assert_eq!(target_of(&format!("infra/{JOB}/0")), on_first_node);
        assert_eq!(
            target_of("infra/nightly"),
            Some(GatewayTarget::Task("nightly".into()))
        );
        for bad in [
            "infra/".to_owned(),
            "/x".to_owned(),
            format!("infra/{JOB}/a"),
            format!("infra/{JOB}/0/1"),
        ] {
            assert_eq!(parse_login(&bad), Err(BadLogin), "{bad}");
        }
    }

    #[test]
    fn a_plain_login_attaches_and_shell_opens_a_new_one() {
        assert_eq!(
            gateway_shell_kind(None, true),
            Ok(GuestChannelKind::Shell {
                pty: true,
                attach: true
            })
        );
        assert_eq!(
            gateway_shell_kind(Some(b"shell"), false),
            Ok(GuestChannelKind::Shell {
                pty: false,
                attach: false
            })
        );
        assert_eq!(gateway_shell_kind(Some(b"true"), true), Err(ONLY_SHELL));
        assert_eq!(
            gateway_shell_kind(Some(b"shell -c id"), true),
            Err(ONLY_SHELL)
        );
    }
}
