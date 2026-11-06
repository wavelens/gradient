/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Result, anyhow};
use nix_bindings::EvalState;
use nix_bindings::eval_cache::AttrCursor;
use serde_json::{Map, Value};

use crate::exec_app::{self, AppKind, EXEC_APP_EXPR};
use crate::ipc::ExecApp;

const PACKAGE_PROGRAM_EXPR: &str = include_str!("run/package.nix");
const DEVSHELL_PROGRAM_EXPR: &str = include_str!("run/devshell.nix");
const PLAIN_APP_PROGRAM_EXPR: &str = "app: app.program";
const DEFAULT_RUN_RETRIES: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunProgram {
    AsDeclared,
    AppProgram,
    Package,
    DevShell,
}

impl RunProgram {
    pub fn of(attr_segments: &[String], app: Option<AppKind>) -> Self {
        if app == Some(AppKind::Exec) {
            Self::AsDeclared
        } else if app == Some(AppKind::Plain) {
            Self::AppProgram
        } else if attr_segments
            .first()
            .is_some_and(|first| first == "devShells")
        {
            Self::DevShell
        } else {
            Self::Package
        }
    }
}

pub fn read_run_app(
    cursor: &AttrCursor,
    state: &EvalState,
    program: RunProgram,
    run_overrides: &str,
) -> Result<ExecApp> {
    let mut app = match program {
        RunProgram::AsDeclared => exec_app::read_exec_app(cursor, state)?,
        RunProgram::AppProgram => read_plain_app(cursor, state)?,
        RunProgram::Package => wrapped_app(cursor, state, PACKAGE_PROGRAM_EXPR)?,
        RunProgram::DevShell => wrapped_app(cursor, state, DEVSHELL_PROGRAM_EXPR)?,
    };
    app.block = with_run_overrides(&app.block, run_overrides).map_err(|e| anyhow!(e))?;
    Ok(app)
}

pub fn read_plain_app(cursor: &AttrCursor, state: &EvalState) -> Result<ExecApp> {
    wrapped_app(cursor, state, PLAIN_APP_PROGRAM_EXPR)
}

fn wrapped_app(cursor: &AttrCursor, state: &EvalState, program_expr: &str) -> Result<ExecApp> {
    let read = state.eval_from_string(&wrapping_expr(program_expr), "/")?;
    let json = read.call(&cursor.value(state)?)?.as_string()?;
    let app = exec_app::parse_app(&json).map_err(|e| anyhow!(e))?;
    exec_app::exec_app(app, None).map_err(|e| anyhow!(e))
}

fn wrapping_expr(program_expr: &str) -> String {
    format!(
        "let program = ({program_expr}); read = ({EXEC_APP_EXPR}); in \
         attr: read {{ type = \"app\"; program = program attr; passthru.gradient = {{ }}; }}"
    )
}

fn with_run_overrides(block: &str, run_overrides: &str) -> Result<String, String> {
    let mut block = json_object(block, "gradient run: exec block is not a JSON object")?;
    let overrides = json_object(
        run_overrides,
        "gradient run: run overrides is not a JSON object",
    )?;
    let declared_ports = block.remove("ports");
    block.extend(overrides);
    merge_ports(&mut block, declared_ports);
    block
        .entry("retries")
        .or_insert_with(|| DEFAULT_RUN_RETRIES.into());
    Ok(Value::Object(block).to_string())
}

fn merge_ports(block: &mut Map<String, Value>, declared: Option<Value>) {
    let Some(Value::Array(declared)) = declared else {
        return;
    };
    let ports = block
        .entry("ports")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(ports) = ports {
        for port in declared {
            if !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
}

fn json_object(json: &str, refusal: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str(json) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(refusal.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(path: &str) -> Vec<String> {
        path.split('.').map(str::to_owned).collect()
    }

    #[test]
    fn an_exec_app_runs_as_declared_even_under_dev_shells() {
        assert_eq!(
            RunProgram::of(&segments("apps.x86_64-linux.train"), Some(AppKind::Exec)),
            RunProgram::AsDeclared
        );
        assert_eq!(
            RunProgram::of(&segments("devShells.x86_64-linux.gpu"), Some(AppKind::Exec)),
            RunProgram::AsDeclared
        );
    }

    #[test]
    fn a_plain_app_runs_its_declared_program_even_under_dev_shells() {
        assert_eq!(
            RunProgram::of(
                &segments("apps.x86_64-linux.frontend"),
                Some(AppKind::Plain)
            ),
            RunProgram::AppProgram
        );
        assert_eq!(
            RunProgram::of(
                &segments("devShells.x86_64-linux.tool"),
                Some(AppKind::Plain)
            ),
            RunProgram::AppProgram
        );
    }

    #[test]
    fn a_dev_shell_attribute_runs_its_environment_and_others_their_main_program() {
        assert_eq!(
            RunProgram::of(&segments("devShells.x86_64-linux.default"), None),
            RunProgram::DevShell
        );
        assert_eq!(
            RunProgram::of(&segments("packages.x86_64-linux.hello"), None),
            RunProgram::Package
        );
        assert_eq!(
            RunProgram::of(&segments("checks.x86_64-linux.devShells"), None),
            RunProgram::Package
        );
    }

    #[test]
    fn run_overrides_replace_declared_fields_and_keep_the_rest() {
        let block = r#"{"cpus":2,"env":{"A":"1"},"ssh":false}"#;
        let merged: Value =
            serde_json::from_str(&with_run_overrides(block, r#"{"cpus":8,"ssh":true}"#).unwrap())
                .unwrap();

        assert_eq!(
            merged,
            serde_json::json!({"cpus": 8, "env": {"A": "1"}, "ssh": true, "retries": 1})
        );
    }

    #[test]
    fn forwarded_ports_add_to_the_declared_ports() {
        let block = r#"{"ports":[8080,6006]}"#;
        let merged: Value =
            serde_json::from_str(&with_run_overrides(block, r#"{"ports":[8888,8080]}"#).unwrap())
                .unwrap();

        assert_eq!(
            merged,
            serde_json::json!({"ports": [8888, 8080, 6006], "retries": 1})
        );
    }

    #[test]
    fn empty_run_overrides_add_only_the_default_retry() {
        let block = r#"{"nodes":2,"network":true}"#;
        let merged: Value =
            serde_json::from_str(&with_run_overrides(block, "{}").unwrap()).unwrap();

        assert_eq!(
            merged,
            serde_json::json!({"nodes": 2, "network": true, "retries": 1})
        );
    }

    #[test]
    fn declared_retries_win_over_the_run_default() {
        let merged: Value =
            serde_json::from_str(&with_run_overrides(r#"{"retries":3}"#, "{}").unwrap()).unwrap();

        assert_eq!(merged, serde_json::json!({"retries": 3}));
    }
}
