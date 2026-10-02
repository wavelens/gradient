/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Daemon,
    MakeTempDir {
        pattern: String,
    },
    Realise {
        derivations: Vec<String>,
        add_root: Option<String>,
    },
    Build {
        derivations: Vec<String>,
    },
    ResolveLink {
        path: String,
    },
    RemoveDir {
        path: String,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("the ssh:// store is not supported by Gradient, use ssh-ng://")]
    LegacyServe,
    #[error("not supported over Gradient SSH: {0}")]
    Unsupported(String),
}

const VALUE_FLAGS: &[&str] = &[
    "--extra-experimental-features",
    "--experimental-features",
    "--max-jobs",
    "-j",
    "--cores",
    "--builders",
    "--log-format",
    "--store",
    "--eval-store",
    "--out-link",
    "-o",
];

pub fn parse(line: &str) -> Result<Command, CommandError> {
    let unsupported = || CommandError::Unsupported(line.to_string());
    let words = shell_words::split(line).map_err(|_| unsupported())?;
    let args: Vec<&str> = words.iter().map(String::as_str).collect();

    match without_env_wrapper(&args) {
        ["nix-daemon", "--stdio", ..] => Ok(Command::Daemon),
        ["nix-store", "--serve", ..] => Err(CommandError::LegacyServe),
        ["mktemp", "-d", "-t", pattern] => Ok(Command::MakeTempDir {
            pattern: pattern.to_string(),
        }),
        ["readlink", "-f", path] if is_tmp(path) => Ok(Command::ResolveLink {
            path: path.to_string(),
        }),
        ["rm", "-rf", path] if is_tmp(path) => Ok(Command::RemoveDir {
            path: path.to_string(),
        }),
        ["nix-store", "--realise" | "-r", rest @ ..] => {
            let (derivations, add_root) = derivations_and_root(rest);
            if derivations.is_empty() {
                return Err(unsupported());
            }

            Ok(Command::Realise {
                derivations,
                add_root,
            })
        }
        ["nix", rest @ ..] if rest.contains(&"build") => {
            let (derivations, _) = derivations_and_root(rest);
            if derivations.is_empty() {
                return Err(unsupported());
            }

            Ok(Command::Build { derivations })
        }
        _ => Err(unsupported()),
    }
}

fn without_env_wrapper<'a>(args: &'a [&'a str]) -> &'a [&'a str] {
    match args {
        ["/bin/sh", "-c", script, "sh", rest @ ..] if is_env_reset(script) => rest,
        _ => args,
    }
}

fn is_env_reset(script: &str) -> bool {
    let Some(assigns) = script
        .strip_prefix("exec /usr/bin/env -i")
        .and_then(|s| s.strip_suffix("\"$@\""))
    else {
        return false;
    };

    assigns.split_whitespace().all(|assign| {
        assign
            .split_once('=')
            .is_some_and(|(key, _)| key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    })
}

fn is_tmp(path: &str) -> bool {
    path.starts_with("/tmp/") && !path.split('/').any(|part| part == "..")
}

fn derivations_and_root(args: &[&str]) -> (Vec<String>, Option<String>) {
    let mut derivations = Vec::new();
    let mut add_root = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--add-root" => {
                add_root = args.get(i + 1).map(|s| s.to_string());
                i += 2;
            }
            "--option" => i += 3,
            flag if VALUE_FLAGS.contains(&flag) => i += 2,
            arg => {
                let path = arg.strip_suffix("^*").unwrap_or(arg);
                if path.starts_with("/nix/store/") && path.ends_with(".drv") {
                    derivations.push(path.to_string());
                }

                i += 1;
            }
        }
    }

    (derivations, add_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DRV: &str = "/nix/store/00000000000000000000000000000000-system.drv";

    #[test]
    fn nix_daemon_stdio_is_the_daemon() {
        assert_eq!(parse("nix-daemon --stdio"), Ok(Command::Daemon));
    }

    #[test]
    fn nixos_rebuild_build_remote_sequence() {
        assert_eq!(
            parse("mktemp -d -t nixos-rebuild.XXXXX"),
            Ok(Command::MakeTempDir {
                pattern: "nixos-rebuild.XXXXX".into()
            })
        );
        assert_eq!(
            parse(&format!(
                "nix-store --realise {DRV} --add-root /tmp/nixos-rebuild.ab12c/f00 --keep-going"
            )),
            Ok(Command::Realise {
                derivations: vec![DRV.into()],
                add_root: Some("/tmp/nixos-rebuild.ab12c/f00".into())
            })
        );
        assert_eq!(
            parse("readlink -f /tmp/nixos-rebuild.ab12c/f00"),
            Ok(Command::ResolveLink {
                path: "/tmp/nixos-rebuild.ab12c/f00".into()
            })
        );
        assert_eq!(
            parse("rm -rf /tmp/nixos-rebuild.ab12c"),
            Ok(Command::RemoveDir {
                path: "/tmp/nixos-rebuild.ab12c".into()
            })
        );
    }

    #[test]
    fn nixos_rebuild_build_remote_flake() {
        assert_eq!(
            parse(&format!(
                "nix --extra-experimental-features 'nix-command flakes' build '{DRV}^*' --print-out-paths --option max-jobs 4"
            )),
            Ok(Command::Build {
                derivations: vec![DRV.into()]
            })
        );
    }

    #[test]
    fn the_legacy_serve_protocol_names_ssh_ng() {
        let err = parse("nix-store --serve --write").expect_err("refused");
        assert_eq!(err, CommandError::LegacyServe);
        assert!(err.to_string().contains("ssh-ng://"));
    }

    #[test]
    fn anything_else_is_unsupported() {
        assert!(matches!(
            parse("bash -c id"),
            Err(CommandError::Unsupported(_))
        ));
        assert!(matches!(
            parse("rm -rf /"),
            Err(CommandError::Unsupported(_))
        ));
        assert!(matches!(
            parse("readlink -f /tmp/../etc/shadow"),
            Err(CommandError::Unsupported(_))
        ));
        assert!(matches!(
            parse("nix-store --realise /etc/passwd"),
            Err(CommandError::Unsupported(_))
        ));
        assert!(matches!(
            parse("nix eval nixpkgs#hello"),
            Err(CommandError::Unsupported(_))
        ));
    }

    #[test]
    fn nixos_rebuild_26_11_wraps_every_command_in_env() {
        let wrap = r#"/bin/sh -c 'exec /usr/bin/env -i  "$@"' sh"#;
        assert_eq!(
            parse(&format!("{wrap} mktemp -d -t nixos-rebuild.XXXXX")),
            Ok(Command::MakeTempDir {
                pattern: "nixos-rebuild.XXXXX".into()
            })
        );
        let with_path = r#"/bin/sh -c 'exec /usr/bin/env -i PATH="${PATH-}" "$@"' sh"#;
        assert_eq!(
            parse(&format!(
                "{with_path} nix-store --realise {DRV} --add-root /tmp/a.b/c"
            )),
            Ok(Command::Realise {
                derivations: vec![DRV.into()],
                add_root: Some("/tmp/a.b/c".into())
            })
        );
    }

    #[test]
    fn a_wrapper_running_another_script_is_unsupported() {
        assert!(matches!(
            parse("/bin/sh -c 'id; exec \"$@\"' sh mktemp -d -t x.XXXXX"),
            Err(CommandError::Unsupported(_))
        ));
    }
}
