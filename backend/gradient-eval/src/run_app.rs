/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Result, anyhow};
use nix_bindings::EvalState;
use nix_bindings::eval_cache::AttrCursor;
use serde_json::{Map, Value};

use crate::exec_app::{self, EXEC_APP_EXPR};
use crate::ipc::ExecApp;

const PACKAGE_PROGRAM_EXPR: &str = include_str!("run/package.nix");
const DEVSHELL_PROGRAM_EXPR: &str = include_str!("run/devshell.nix");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunProgram {
    AsDeclared,
    Package,
    DevShell,
}

impl RunProgram {
    pub fn of(attr_segments: &[String], is_exec_app: bool) -> Self {
        if is_exec_app {
            Self::AsDeclared
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
        RunProgram::Package => wrapped_app(cursor, state, PACKAGE_PROGRAM_EXPR)?,
        RunProgram::DevShell => wrapped_app(cursor, state, DEVSHELL_PROGRAM_EXPR)?,
    };
    app.block = with_run_overrides(&app.block, run_overrides).map_err(|e| anyhow!(e))?;
    Ok(app)
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
    block.extend(overrides);
    Ok(Value::Object(block).to_string())
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
            RunProgram::of(&segments("apps.x86_64-linux.train"), true),
            RunProgram::AsDeclared
        );
        assert_eq!(
            RunProgram::of(&segments("devShells.x86_64-linux.gpu"), true),
            RunProgram::AsDeclared
        );
    }

    #[test]
    fn a_dev_shell_attribute_runs_its_environment_and_others_their_main_program() {
        assert_eq!(
            RunProgram::of(&segments("devShells.x86_64-linux.default"), false),
            RunProgram::DevShell
        );
        assert_eq!(
            RunProgram::of(&segments("packages.x86_64-linux.hello"), false),
            RunProgram::Package
        );
        assert_eq!(
            RunProgram::of(&segments("checks.x86_64-linux.devShells"), false),
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
            serde_json::json!({"cpus": 8, "env": {"A": "1"}, "ssh": true})
        );
    }

    #[test]
    fn empty_run_overrides_leave_an_exec_app_untouched() {
        let block = r#"{"nodes":2,"network":true}"#;
        let merged: Value =
            serde_json::from_str(&with_run_overrides(block, "{}").unwrap()).unwrap();

        assert_eq!(merged, serde_json::json!({"nodes": 2, "network": true}));
    }
}
