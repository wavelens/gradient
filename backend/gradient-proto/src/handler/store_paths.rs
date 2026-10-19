/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::StorePathError;
use gradient_types::StorePathMapper;
use gradient_wire::messages::ClientMessage;
use gradient_wire::types::{InputOutcome, JobUpdateKind, UploadMetadata, UploadObject};
use serde_json::Value;

type MapText = fn(&StorePathMapper, &str) -> Result<String, StorePathError>;

struct TextMapper<'a> {
    mapper: &'a StorePathMapper,
    map: MapText,
}

impl TextMapper<'_> {
    fn path(&self, text: &mut String) -> Result<(), StorePathError> {
        if !text.is_empty() {
            *text = (self.map)(self.mapper, text)?;
        }
        Ok(())
    }

    fn paths(&self, texts: &mut [String]) -> Result<(), StorePathError> {
        texts.iter_mut().try_for_each(|text| self.path(text))
    }

    fn json_store_path(&self, json: &mut String) -> Result<(), StorePathError> {
        let Ok(mut value) = serde_json::from_str::<Value>(json) else {
            return Ok(());
        };
        let Some(Value::String(path)) = value.get_mut("storePath") else {
            return Ok(());
        };
        self.path(path)?;
        *json = value.to_string();
        Ok(())
    }
}

fn inbound(mapper: &StorePathMapper) -> TextMapper<'_> {
    TextMapper {
        mapper,
        map: StorePathMapper::inbound_text,
    }
}

pub(super) fn inbound_message(
    mut msg: ClientMessage,
    mapper: &StorePathMapper,
) -> Result<ClientMessage, StorePathError> {
    let paths = inbound(mapper);
    match &mut msg {
        ClientMessage::JobUpdate { update, .. } => inbound_update(update, &paths)?,
        ClientMessage::JobFailed { missing_paths, .. } => paths.paths(missing_paths)?,
        ClientMessage::CacheQuery { paths: queried, .. } => paths.paths(queried)?,
        ClientMessage::QueryKnownDerivations { drv_paths, .. }
        | ClientMessage::ImportRequest { drv_paths, .. } => paths.paths(drv_paths)?,
        ClientMessage::NarRequest {
            paths: requested, ..
        } => paths.paths(requested)?,
        ClientMessage::NarRequestResume { store_path, .. }
        | ClientMessage::UploadRequest {
            object: UploadObject::Nar { store_path },
            ..
        } => paths.path(store_path)?,
        ClientMessage::InputRequest { request, .. } => paths.json_store_path(request)?,
        ClientMessage::InputFetched {
            outcome: InputOutcome::Fetched { response },
            ..
        } => paths.json_store_path(response)?,
        _ => {}
    }
    Ok(msg)
}

fn inbound_update(update: &mut JobUpdateKind, paths: &TextMapper) -> Result<(), StorePathError> {
    match update {
        JobUpdateKind::EvalResult { derivations, .. } => {
            for derivation in derivations {
                paths.path(&mut derivation.drv_path)?;
                for output in &mut derivation.outputs {
                    paths.path(&mut output.path)?;
                }
                paths.paths(&mut derivation.dependencies)?;
                paths.paths(&mut derivation.input_sources)?;
            }
        }
        JobUpdateKind::EvalStats(report) => {
            for node in &mut report.flake_nodes {
                if let Some(drv_path) = &mut node.drv_path {
                    paths.path(drv_path)?;
                }
            }
        }
        JobUpdateKind::FetchResult {
            flake_source: Some(flake_source),
        } => paths.path(flake_source)?,
        JobUpdateKind::BuildOutput { outputs, .. } => {
            for output in outputs {
                paths.path(&mut output.store_path)?;
                paths.path(&mut output.hash)?;
                for product in &mut output.products {
                    paths.path(&mut product.path)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn inbound_upload_metadata(
    mut metadata: UploadMetadata,
    mapper: &StorePathMapper,
) -> Result<UploadMetadata, StorePathError> {
    if let UploadMetadata::Nar(meta) = &mut metadata {
        let paths = inbound(mapper);
        paths.paths(&mut meta.references)?;
        if let Some(deriver) = &mut meta.deriver {
            paths.path(deriver)?;
        }
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};
    use gradient_types::ids::ProjectId;
    use gradient_types::{MappingContext, StorePathMapper};
    use gradient_wire::types::{
        BuildOutput, BuildProduct, DerivationOutput, DiscoveredDerivation, JobUpdateKind,
        NarUploadMetadata, UploadMetadata,
    };
    use std::sync::Arc;

    const O: &str = EXAMPLE_ORIGINAL_HASH;
    const S: &str = EXAMPLE_STORED_HASH;

    fn mapper() -> StorePathMapper {
        StorePathMapper::new(
            Arc::new(RotatedHashMapping),
            MappingContext::Project(ProjectId::nil()),
        )
    }

    #[test]
    fn every_path_of_an_eval_result_is_mapped_inbound() {
        let derivation = DiscoveredDerivation {
            attr: "a".into(),
            drv_path: format!("/nix/store/{O}-a.drv"),
            outputs: vec![
                DerivationOutput {
                    name: "out".into(),
                    path: format!("/nix/store/{O}-a"),
                },
                DerivationOutput {
                    name: "dev".into(),
                    path: String::new(),
                },
            ],
            dependencies: vec![format!("/nix/store/{O}-b.drv")],
            input_sources: vec![format!("/nix/store/{O}-src")],
            ..Default::default()
        };
        let msg = ClientMessage::JobUpdate {
            job_id: "j1".into(),
            assignment_id: "a1".into(),
            update: JobUpdateKind::EvalResult {
                derivations: vec![derivation],
                warnings: vec![],
                errors: vec![],
            },
        };
        let ClientMessage::JobUpdate {
            update: JobUpdateKind::EvalResult { derivations, .. },
            ..
        } = inbound_message(msg, &mapper()).unwrap()
        else {
            panic!("an EvalResult stays an EvalResult")
        };
        assert_eq!(derivations[0].attr, "a");
        assert_eq!(derivations[0].drv_path, format!("/nix/store/{S}-a.drv"));
        assert_eq!(derivations[0].outputs[0].path, format!("/nix/store/{S}-a"));
        assert_eq!(derivations[0].outputs[1].path, "");
        assert_eq!(
            derivations[0].dependencies,
            vec![format!("/nix/store/{S}-b.drv")]
        );
        assert_eq!(
            derivations[0].input_sources,
            vec![format!("/nix/store/{S}-src")]
        );
    }

    #[test]
    fn a_build_product_keeps_its_path_inside_the_output() {
        let msg = ClientMessage::JobUpdate {
            job_id: "j1".into(),
            assignment_id: "a1".into(),
            update: JobUpdateKind::BuildOutput {
                build_id: "b1".into(),
                outputs: vec![BuildOutput {
                    name: "out".into(),
                    store_path: format!("/nix/store/{O}-a"),
                    hash: O.into(),
                    nar_size: None,
                    nar_hash: None,
                    products: vec![BuildProduct {
                        file_type: "doc".into(),
                        subtype: "pdf".into(),
                        name: "doc.pdf".into(),
                        path: format!("/nix/store/{O}-a/doc.pdf"),
                        size: None,
                    }],
                }],
                metrics: None,
                substituted: false,
            },
        };
        let ClientMessage::JobUpdate {
            update: JobUpdateKind::BuildOutput { outputs, .. },
            ..
        } = inbound_message(msg, &mapper()).unwrap()
        else {
            panic!("a BuildOutput stays a BuildOutput")
        };
        assert_eq!(outputs[0].store_path, format!("/nix/store/{S}-a"));
        assert_eq!(outputs[0].hash, S);
        assert_eq!(
            outputs[0].products[0].path,
            format!("/nix/store/{S}-a/doc.pdf")
        );
    }

    #[test]
    fn an_input_request_maps_its_store_path_key() {
        let request = |hash: &str| {
            format!(
                r#"{{"builtin":"fetchTree","args":{{"type":"tarball","url":"https://example.com/x.tar.gz"}},"storePath":"/nix/store/{hash}-input"}}"#
            )
        };
        let msg = ClientMessage::InputRequest {
            job_id: "j1".into(),
            request_id: "r1".into(),
            request: request(O),
        };
        let ClientMessage::InputRequest {
            request: mapped, ..
        } = inbound_message(msg, &mapper()).unwrap()
        else {
            panic!("an InputRequest stays an InputRequest")
        };
        let json = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
        assert_eq!(json(&mapped), json(&request(S)));
    }

    #[test]
    fn upload_metadata_maps_its_references_and_deriver() {
        let metadata = UploadMetadata::Nar(Box::new(NarUploadMetadata {
            file_hash: "sha256:f".into(),
            file_size: 1,
            nar_size: 1,
            nar_hash: "sha256:n".into(),
            references: vec![format!("{O}-dep")],
            deriver: Some(format!("{O}-a.drv")),
            ca: None,
            multipart: None,
        }));
        let UploadMetadata::Nar(meta) = inbound_upload_metadata(metadata, &mapper()).unwrap()
        else {
            panic!("NAR metadata stays NAR metadata")
        };
        assert_eq!(meta.references, vec![format!("{S}-dep")]);
        assert_eq!(meta.deriver, Some(format!("{S}-a.drv")));
        assert_eq!(meta.file_hash, "sha256:f");
    }

    #[test]
    fn a_short_hash_is_malformed() {
        let msg = ClientMessage::NarRequest {
            job_id: "j1".into(),
            paths: vec!["/nix/store/abc-a".into()],
        };
        assert!(inbound_message(msg, &mapper()).is_err());
    }
}
