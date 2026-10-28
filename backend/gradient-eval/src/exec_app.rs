/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Result, anyhow};
use nix_bindings::EvalState;
use nix_bindings::eval_cache::AttrCursor;
use serde::Deserialize;

use crate::ipc::{ExecApp, ExecAppFile};
use crate::strip_nix_store_prefix;

pub const EXEC_APP_EXPR: &str = r#"app: let
  g = app.passthru.gradient;
  ctx = builtins.getContext app.program;
  problem =
    if !builtins.isAttrs g then "must be an attribute set, not a ${builtins.typeOf g}"
    else if g ? image && !(builtins.isAttrs g.image && g.image ? config.system.build.toplevel)
    then "image has no config.system.build.toplevel"
    else null;
in builtins.unsafeDiscardStringContext (builtins.toJSON (if problem != null then { inherit problem; } else {
  block = removeAttrs g [ "image" "sops" ];
  program = app.program;
  programDrvs = builtins.filter (k: ctx.${k} ? outputs) (builtins.attrNames ctx);
  sops = map (p: {
    path = if builtins.isPath p then toString p else null;
    content = if builtins.isPath p then builtins.readFile p else "";
  }) (g.sops or [ ]);
}))"#;

pub const MAX_SOPS_FILE_BYTES: usize = 1 << 20;

const IMAGE_TOPLEVEL: [&str; 4] = ["config", "system", "build", "toplevel"];

#[derive(Deserialize)]
#[serde(untagged)]
enum AppJson {
    Refused { problem: String },
    Read(ReadApp),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadApp {
    block: serde_json::Value,
    program: String,
    program_drvs: Vec<String>,
    #[serde(default)]
    sops: Vec<ReadSopsFile>,
}

#[derive(Deserialize)]
struct ReadSopsFile {
    path: Option<String>,
    content: String,
}

pub fn is_exec_app(cursor: &AttrCursor) -> Result<bool> {
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

pub fn read_exec_app(cursor: &AttrCursor, state: &EvalState) -> Result<ExecApp> {
    let json = app_json(cursor, state).map_err(|e| anyhow!("passthru.gradient: {e}"))?;
    let app = parse_app(&json).map_err(|e| anyhow!(e))?;
    let image = image_drv(cursor, state).map_err(|e| anyhow!("passthru.gradient: image: {e}"))?;
    exec_app(app, image.as_deref()).map_err(|e| anyhow!(e))
}

fn app_json(cursor: &AttrCursor, state: &EvalState) -> Result<String> {
    let read = state.eval_from_string(EXEC_APP_EXPR, "/")?;
    Ok(read.call(&cursor.value(state)?)?.as_string()?)
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
        at = at
            .maybe_get_attr(name)?
            .ok_or_else(|| anyhow!("has no {}", IMAGE_TOPLEVEL.join(".")))?;
    }

    Ok(Some(at.drv_path(state)?))
}

fn parse_app(json: &str) -> Result<ReadApp, String> {
    match serde_json::from_str(json) {
        Ok(AppJson::Read(app)) => Ok(app),
        Ok(AppJson::Refused { problem }) => Err(format!("passthru.gradient: {problem}")),
        Err(e) => Err(format!("passthru.gradient: unreadable app: {e}")),
    }
}

fn exec_app(app: ReadApp, image_drv: Option<&str>) -> Result<ExecApp, String> {
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

    let sops = app
        .sops
        .into_iter()
        .map(sops_file)
        .collect::<Result<_, _>>()?;

    Ok(ExecApp {
        block: app.block.to_string(),
        program_drv: strip_nix_store_prefix(program_drv),
        image_drv: image_drv.map(strip_nix_store_prefix),
        program: app.program,
        sops,
    })
}

fn sops_file(file: ReadSopsFile) -> Result<ExecAppFile, String> {
    let stored = file
        .path
        .ok_or("passthru.gradient: sops entry is not a path")?;
    let path = stored
        .strip_prefix("/nix/store/")
        .and_then(|rest| rest.split_once('/'))
        .map(|(_, relative)| relative.to_owned())
        .ok_or_else(|| format!("passthru.gradient: sops file {stored} is not in the repository"))?;
    if file.content.len() > MAX_SOPS_FILE_BYTES {
        return Err(format!(
            "passthru.gradient: sops file {path} is larger than 1 MiB"
        ));
    }

    Ok(ExecAppFile {
        path,
        content: file.content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_app_from_json(json: &str, image_drv: Option<&str>) -> Result<ExecApp, String> {
        exec_app(parse_app(json)?, image_drv)
    }

    #[test]
    fn a_problem_found_by_the_expression_is_a_field_error() {
        let json = r#"{"problem":"must be an attribute set, not a string"}"#;
        assert_eq!(
            exec_app_from_json(json, None).unwrap_err(),
            "passthru.gradient: must be an attribute set, not a string"
        );
    }

    #[test]
    fn a_program_outside_the_store_is_a_field_error() {
        let json = r#"{"block":{},"program":"/bin/sh","programDrvs":[]}"#;
        assert_eq!(
            exec_app_from_json(json, None).unwrap_err(),
            "passthru.gradient: program \"/bin/sh\" is not a store path"
        );
    }

    #[test]
    fn a_program_must_come_from_exactly_one_derivation() {
        let two = r#"{"block":{},"program":"/nix/store/aaaa-x/bin/x","programDrvs":["/nix/store/a.drv","/nix/store/b.drv"]}"#;
        assert!(
            exec_app_from_json(two, None)
                .unwrap_err()
                .contains("exactly one derivation")
        );
        let one = r#"{"block":{"network":true},"program":"/nix/store/aaaa-x/bin/x","programDrvs":["/nix/store/aaaa-x.drv"]}"#;
        let app = exec_app_from_json(one, Some("/nix/store/bbbb-nixos-system.drv")).unwrap();
        assert_eq!(
            (app.program_drv.as_str(), app.image_drv.as_deref()),
            ("aaaa-x.drv", Some("bbbb-nixos-system.drv"))
        );
        assert_eq!(app.block, r#"{"network":true}"#);
    }

    #[test]
    fn a_sops_file_is_named_relative_to_the_repository() {
        let json = r#"{"block":{},"program":"/nix/store/aaaa-x/bin/x","programDrvs":["/nix/store/aaaa-x.drv"],"image":null,
            "sops":[{"path":"/nix/store/bbbb-source/secrets/train.yaml","content":"wandb-token: ENC[x]"}]}"#;
        let app = exec_app_from_json(json, None).unwrap();
        assert_eq!(
            app.sops,
            vec![ExecAppFile {
                path: "secrets/train.yaml".into(),
                content: "wandb-token: ENC[x]".into()
            }]
        );
    }

    #[test]
    fn a_sops_entry_that_is_not_a_path_is_a_field_error() {
        let json = r#"{"block":{},"program":"/nix/store/aaaa-x/bin/x","programDrvs":["/nix/store/aaaa-x.drv"],"image":null,"sops":[{"path":null,"content":""}]}"#;
        assert_eq!(
            exec_app_from_json(json, None).unwrap_err(),
            "passthru.gradient: sops entry is not a path"
        );
    }
}
