/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::shape::shape;
use gradient_fixtures::store_path_content_variants::{BUILD_A, BUILD_B};
use gradient_util::nar::single_file_nar;
use gradient_wire::types::DerivationOutput;

const INPUT_SOURCE: &str = "/nix/store/0c0v1aqy6l8gwn3bn7qn8rcnzvz6phhj-builder.sh";
const OUTPUT: &str = "/nix/store/1b1v1aqy6l8gwn3bn7qn8rcnzvz6phhj-hello";

fn drv_text(system: &str, padding: usize) -> Vec<u8> {
    format!(
        r#"Derive([("out","{OUTPUT}","","")],[],["{INPUT_SOURCE}"],"{system}","/bin/sh",[],[("name","hello"),("padding","{padding}"),("timeout","3600")])"#,
        padding = "x".repeat(padding)
    )
    .into_bytes()
}

fn store_hash_of(text: &[u8]) -> String {
    let references: StorePathSet =
        [
            NixStorePath::from_base_path(INPUT_SOURCE.trim_start_matches("/nix/store/"))
                .expect("a valid input source"),
        ]
        .into_iter()
        .collect();
    let path = path_for(
        "hello.drv",
        &ContentAddress::Text(Sha256::digest(text)),
        &references,
    )
    .expect("a text store path")
    .to_string();
    path.split_once('-').expect("a base path").0.to_owned()
}

struct StoredDrv {
    db: TestDatabase,
    nar_storage: NarStore,
    trusting: ProjectId,
    other: ProjectId,
    drv_path: String,
    hash: String,
}

impl StoredDrv {
    async fn new(path_text: &[u8], stored_text: &[u8]) -> Self {
        let hash = store_hash_of(path_text);
        let (trusting, other) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = shape()
            .evaluation(trusting)
            .derivation(BUILD_A)
            .job()
            .cached_path(&hash, "hello.drv")
            .evaluation(other)
            .derivation(BUILD_B)
            .job()
            .into_database()
            .await;
        let dir = std::env::temp_dir().join(format!("gradient-graph-drv-{}", uuid::Uuid::now_v7()));
        let nar_storage = NarStore::local(&dir).expect("the NAR store opens");
        let compressed = zstd::encode_all(single_file_nar(stored_text, false).as_slice(), 0)
            .expect("zstd compresses the NAR");
        nar_storage
            .put(&format!("{hash}-abc"), compressed)
            .await
            .expect("the NAR is stored");
        Self {
            trusting: db.project_of(trusting).await,
            other: db.project_of(other).await,
            db,
            nar_storage,
            drv_path: format!("/nix/store/{hash}-hello.drv"),
            hash,
        }
    }

    async fn store_older_drv_variant_for_same_store_path_that_ranks_first(
        &self,
        stored_text: &[u8],
    ) {
        let hash = &self.hash;
        let key = format!("{hash}-{}", uuid::Uuid::now_v7().simple());
        let compressed = zstd::encode_all(single_file_nar(stored_text, false).as_slice(), 0)
            .expect("zstd compresses the NAR");
        self.nar_storage
            .put(&key, compressed)
            .await
            .expect("The store can contain an additional variant.");
        self.db
            .execute(&format!(
                "WITH copy AS (INSERT INTO cached_path \
                   SELECT (jsonb_populate_record(NULL::cached_path, to_jsonb(c) || jsonb_build_object( \
                     'id', uuidv7(), 'nar_object_storage_key', '{key}', 'nar_hash', 'sha256:{key}', \
                     'created_at', (SELECT min(created_at) FROM cached_path WHERE hash = '{hash}') \
                                   - interval '1 minute'))).* \
                   FROM cached_path c WHERE c.nar_object_storage_key = '{hash}-abc' RETURNING id) \
                 INSERT INTO store_path_content_producer (id, cached_path, kind, worker_registration, created_at) \
                 SELECT uuidv7(), copy.id, p.kind, p.worker_registration, p.created_at \
                 FROM copy, store_path_content_producer p JOIN cached_path c ON c.id = p.cached_path \
                 WHERE c.nar_object_storage_key = '{hash}-abc'"
            ))
            .await;
    }

    async fn read(
        &self,
        project: ProjectId,
    ) -> anyhow::Result<Option<StorePathVerifiedDerivationRecord>> {
        load_verified_trusted_derivation_record(
            &self.db.connection(),
            &self.nar_storage,
            Arc::new(IdentityMapping),
            project,
            &self.drv_path,
        )
        .await
    }
}

#[tokio::test]
async fn test_drv_file_with_matching_text_gives_derivation_record() {
    let text = drv_text("x86_64-linux", 0);
    let stored = StoredDrv::new(&text, &text).await;

    let record = stored
        .read(stored.trusting)
        .await
        .expect("the read succeeds")
        .expect("The project trusted the stored variant. The text matched its store path, so the reader returned the record.")
        .to_discovered_derivation();

    assert_eq!(
        (
            record.architecture.as_str(),
            record.outputs,
            record.input_sources,
            record.timeout_secs
        ),
        (
            "x86_64-linux",
            vec![DerivationOutput {
                name: "out".into(),
                path: OUTPUT.into(),
            }],
            vec![INPUT_SOURCE.to_owned()],
            Some(3600)
        ),
        "The record has the architecture, outputs, input sources and build timeout from the file."
    );
    stored.db.finish().await;
}

#[tokio::test]
async fn test_drv_file_of_variant_the_project_does_not_trust_is_not_found() {
    let text = drv_text("x86_64-linux", 0);
    let stored = StoredDrv::new(&text, &text).await;

    let record = stored.read(stored.other).await.expect("the read succeeds");

    assert!(
        record.is_none(),
        "Projects that don't trust the only stored variant produce no record."
    );
    stored.db.finish().await;
}

#[tokio::test]
async fn test_drv_file_whose_text_does_not_hash_to_its_store_path_is_refused() {
    let stored = StoredDrv::new(&drv_text("x86_64-linux", 0), &drv_text("aarch64-linux", 0)).await;

    let refused = stored.read(stored.trusting).await;

    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.to_string().contains("does not match its store path")),
        "Saved files whose text hash does not match the store path fail the assertion."
    );
    stored.db.finish().await;
}

#[tokio::test]
async fn test_drv_file_reader_returns_trusted_variant_whose_text_hashes_to_its_store_path_when_mismatching_variant_ranks_higher()
 {
    let honest = drv_text("x86_64-linux", 0);
    let stored = StoredDrv::new(&honest, &honest).await;
    stored
        .store_older_drv_variant_for_same_store_path_that_ranks_first(&drv_text("aarch64-linux", 0))
        .await;

    let record = stored
        .read(stored.trusting)
        .await
        .expect("the read succeeds")
        .expect("The validation checks accept the honest lower-ranked variant, so the reader can return it.")
        .to_discovered_derivation();

    assert_eq!(
        record.architecture, "x86_64-linux",
        "The reader can skip the mismatching higher-ranked variant and return the record of the honest variant."
    );
    stored.db.finish().await;
}

#[tokio::test]
async fn test_drv_file_refuses_too_many_mismatching_variants_above_try_bound_ranked_over_honest() {
    let honest = drv_text("x86_64-linux", 0);
    let stored = StoredDrv::new(&honest, &honest).await;
    for _ in 0..4 {
        stored
            .store_older_drv_variant_for_same_store_path_that_ranks_first(&drv_text(
                "aarch64-linux",
                0,
            ))
            .await;
    }

    let refused = stored.read(stored.trusting).await;

    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.to_string().contains("does not match its store path")),
        "The read path can check only up to the maximum number of trusted variants. Honest variants ranked below that many mismatched variants receive a refusal."
    );
    stored.db.finish().await;
}

#[tokio::test]
async fn test_drv_file_with_nar_above_the_memory_limit_is_refused() {
    let padding =
        usize::try_from(MAX_DRV_FILE_BYTES_READ_INTO_MEMORY).expect("the limit fits in memory");
    let text = drv_text("x86_64-linux", padding);
    let stored = StoredDrv::new(&text, &text).await;

    let refused = stored.read(stored.trusting).await;

    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.to_string().contains("is larger than")),
        "NARs that exceed the memory limit never reach the parser."
    );
    stored.db.finish().await;
}

#[test]
fn a_derivation_read_from_the_cache_names_stored_paths() {
    use gradient_derivation::{DerivationOutput, InputDrv};
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH, RotatedHashMapping};

    let (o, s) = (EXAMPLE_ORIGINAL_HASH, EXAMPLE_STORED_HASH);
    let mapper = StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
    let output = |path: String| DerivationOutput {
        name: "out".into(),
        path,
        hash_algo: String::new(),
        hash: String::new(),
    };
    let drv = Derivation {
        outputs: vec![output(format!("/nix/store/{o}-hello"))],
        input_derivations: vec![InputDrv::new(
            format!("/nix/store/{o}-dep.drv"),
            vec!["out".into()],
        )],
        input_sources: vec![format!("/nix/store/{o}-src")],
        system: "x86_64-linux".into(),
        builder: format!("/nix/store/{o}-bash/bin/bash"),
        args: Vec::new(),
        environment: Default::default(),
    };

    let stored = with_stored_paths(&mapper, drv).expect("stored");

    assert_eq!(
        stored.outputs,
        vec![output(format!("/nix/store/{s}-hello"))]
    );
    assert_eq!(
        stored.input_derivations,
        vec![InputDrv::new(
            format!("/nix/store/{s}-dep.drv"),
            vec!["out".to_owned()]
        )]
    );
    assert_eq!(stored.input_sources, vec![format!("/nix/store/{s}-src")]);
    assert_eq!(stored.builder, format!("/nix/store/{o}-bash/bin/bash"));
}

#[test]
fn mapping_parsed_derivation_paths_to_stored_cache_paths_fails_for_input_subpaths_and_parent_segments()
 {
    use gradient_derivation::InputDrv;
    use gradient_entity::{EXAMPLE_ORIGINAL_HASH, RotatedHashMapping};

    let o = EXAMPLE_ORIGINAL_HASH;
    let mapper = StorePathMapper::for_cache(Arc::new(RotatedHashMapping), CacheId::nil());
    let derivation = |input_derivations: Vec<InputDrv>, input_sources: Vec<String>| Derivation {
        outputs: Vec::new(),
        input_derivations,
        input_sources,
        system: "x86_64-linux".into(),
        builder: String::new(),
        args: Vec::new(),
        environment: Default::default(),
    };
    for path in [
        format!("/nix/store/{o}-dep.drv/sub"),
        format!("/nix/store/{o}-dep.drv/../x"),
    ] {
        let input = InputDrv::new(path.clone(), vec!["out".into()]);
        assert!(
            with_stored_paths(&mapper, derivation(vec![input], Vec::new())).is_err(),
            "{path}"
        );
        assert!(
            with_stored_paths(&mapper, derivation(Vec::new(), vec![path.clone()])).is_err(),
            "{path}"
        );
    }
}

#[test]
fn building_map_fails_when_verified_derivation_drv_path_is_not_store_path() {
    let derivation_with_invalid_drv_path = StorePathVerifiedDerivationRecord {
        derivation: DiscoveredDerivation {
            drv_path: "store_path_without_separator.drv".to_owned(),
            ..Default::default()
        },
        original_client_derivation_store_path: String::new(),
        verified_derivation_file_bytes: Arc::from([]),
    };

    map_verified_derivation_records_by_store_path([&derivation_with_invalid_drv_path])
        .expect_err("Records with paths that don't parse make the map fail.");
}
