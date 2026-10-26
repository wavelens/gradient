/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Result, anyhow};
use nix_bindings::EvalState;
use nix_bindings::eval_cache::AttrCursor;
use serde::Deserialize;

use crate::ipc::RunnerApp;
use crate::strip_nix_store_prefix;

pub const RUNNER_APP_EXPR: &str = r#"app: let g = app.passthru.gradient; ctx = builtins.getContext app.program;
in builtins.unsafeDiscardStringContext (builtins.toJSON {
  block = removeAttrs g [ "image" "sops" ];
  program = app.program;
  programDrvs = builtins.filter (k: ctx.${k} ? outputs) (builtins.attrNames ctx);
})"#;

const IMAGE_TOPLEVEL: [&str; 4] = ["config", "system", "build", "toplevel"];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppJson {
    block: serde_json::Value,
    program: String,
    program_drvs: Vec<String>,
}

pub fn is_runner_app(cursor: &AttrCursor) -> Result<bool> {
    let Some(kind) = cursor.maybe_get_attr("type")? else {
        return Ok(false);
    };
    if kind.string()? != "app" {
        return Ok(false);
    }

    Ok(cursor
        .maybe_get_attr("passthru")?
        .map(|passthru| passthru.maybe_get_attr("gradient"))
        .transpose()?
        .flatten()
        .is_some())
}

pub fn read_runner_app(cursor: &AttrCursor, state: &EvalState) -> Result<RunnerApp> {
    let read = state.eval_from_string(RUNNER_APP_EXPR, "/")?;
    let json = read.call(&cursor.value(state)?)?.as_string()?;
    let image = image_drv(cursor, state)?;
    runner_app_from_json(&json, image.as_deref()).map_err(|e| anyhow!(e))
}

fn image_drv(cursor: &AttrCursor, state: &EvalState) -> Result<Option<String>> {
    let Some(mut at) = cursor
        .maybe_get_attr("passthru")?
        .map(|passthru| passthru.maybe_get_attr("gradient"))
        .transpose()?
        .flatten()
        .map(|gradient| gradient.maybe_get_attr("image"))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    for name in IMAGE_TOPLEVEL {
        at = at.maybe_get_attr(name)?.ok_or_else(|| {
            anyhow!(
                "passthru.gradient: image has no {}",
                IMAGE_TOPLEVEL.join(".")
            )
        })?;
    }

    Ok(Some(at.drv_path(state)?))
}

pub fn runner_app_from_json(json: &str, image_drv: Option<&str>) -> Result<RunnerApp, String> {
    let app: AppJson = serde_json::from_str(json)
        .map_err(|e| format!("passthru.gradient: unreadable app: {e}"))?;
    if !app.program.starts_with("/nix/store/") {
        return Err(format!(
            "passthru.gradient: program {:?} is not a store path",
            app.program
        ));
    }

    let [program_drv] = app.program_drvs.as_slice() else {
        return Err(format!(
            "passthru.gradient: program {:?} must come from exactly one derivation, found {}",
            app.program,
            app.program_drvs.len()
        ));
    };

    Ok(RunnerApp {
        block: app.block.to_string(),
        program_drv: strip_nix_store_prefix(program_drv),
        image_drv: image_drv.map(strip_nix_store_prefix),
        program: app.program,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_outside_the_store_is_a_field_error() {
        let json = r#"{"block":{},"program":"/bin/sh","programDrvs":[]}"#;
        assert_eq!(
            runner_app_from_json(json, None).unwrap_err(),
            "passthru.gradient: program \"/bin/sh\" is not a store path"
        );
    }

    #[test]
    fn a_program_must_come_from_exactly_one_derivation() {
        let two = r#"{"block":{},"program":"/nix/store/aaaa-x/bin/x","programDrvs":["/nix/store/a.drv","/nix/store/b.drv"]}"#;
        assert!(
            runner_app_from_json(two, None)
                .unwrap_err()
                .contains("exactly one derivation")
        );
        let one = r#"{"block":{"network":true},"program":"/nix/store/aaaa-x/bin/x","programDrvs":["/nix/store/aaaa-x.drv"]}"#;
        let app = runner_app_from_json(one, Some("/nix/store/bbbb-nixos-system.drv")).unwrap();
        assert_eq!(
            (app.program_drv.as_str(), app.image_drv.as_deref()),
            ("aaaa-x.drv", Some("bbbb-nixos-system.drv"))
        );
        assert_eq!(app.block, r#"{"network":true}"#);
    }
}
