// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

mod utils;

use nix_daemon::{
    nix::DaemonStore, BuildMode, ClientSettings, Progress, ProgressExt, Stderr, Store, Verbosity,
};
use std::io::Write;
use std::{collections::HashMap, process::Stdio};
use tokio_test::io::Builder;
use tracing::warn;
use utils::init_logging;

const INVALID_STORE_PATH: &'static str =
    "/nix/store/ffffffffffffffffffffffffffffffff-invalid-1.0.0";

// Find the store path for the system's `nix` command.
fn find_nix_in_store() -> String {
    for dir in std::env::var("PATH").unwrap().split(":") {
        let path = std::path::Path::new(dir).join("nix");
        match path.try_exists() {
            Ok(true) => {
                return path
                    // /run/current-system/sw/bin/nix
                    .canonicalize()
                    .unwrap()
                    // /nix/store/ffffffffffffffffffffffffffffffff-nix-2.18.1/bin/nix
                    .parent()
                    .unwrap()
                    // /nix/store/ffffffffffffffffffffffffffffffff-nix-2.18.1/bin
                    .parent()
                    .unwrap()
                    // /nix/store/ffffffffffffffffffffffffffffffff-nix-2.18.1
                    .to_str()
                    .expect("invalid path")
                    .to_owned();
            }
            Ok(false) => {}
            Err(err @ std::io::Error { .. })
                if err.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(err) => panic!("try_exists() failed: {}", err),
        }
    }
    panic!("No `nix` command in $PATH");
}

// Instantiates a derivation that creates a known derivation in the store.
fn create_known_test_file() -> String {
    let out = std::process::Command::new("nix-instantiate")
        .arg("-E")
        .arg(
            "derivation {
                name = \"nix-daemon-fixed-test-file\";
                builder = \"/bin/sh\";
                system = builtins.currentSystem;
            }",
        )
        .output()
        .expect("Couldn't create known test derivation");
    String::from_utf8(out.stdout)
        .expect("Invalid nix-instantiate output")
        .trim()
        .to_owned()
}

#[tokio::test]
async fn test_set_options() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    store
        .set_options(ClientSettings {
            keep_failed: false,
            keep_going: false,
            try_fallback: false,
            verbosity: Verbosity::Vomit,
            max_build_jobs: 2,
            max_silent_time: 60,
            verbose_build: true,
            build_cores: 69,
            use_substitutes: false,
            overrides: HashMap::new(),
        })
        .result()
        .await
        .expect("SetOptions Result failed");
}

#[tokio::test]
async fn test_is_valid_path_false() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store.is_valid_path(INVALID_STORE_PATH).split().await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(false, r.unwrap());
}
#[tokio::test]
async fn test_is_valid_path_true() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store.is_valid_path(find_nix_in_store()).split().await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(true, r.unwrap());
}

#[tokio::test]
async fn test_query_valid_paths_false() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    store
        .set_options(ClientSettings {
            use_substitutes: false, // Disable substituters.
            ..Default::default()
        })
        .result()
        .await
        .expect("SetOptions failed");
    let r = store
        .query_valid_paths(&[INVALID_STORE_PATH], true)
        .result()
        .await;
    assert_eq!(Vec::<String>::new(), r.unwrap());
}
#[tokio::test]
async fn test_query_valid_paths_true() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let nix_path = find_nix_in_store();
    let r = store.query_valid_paths(&[&nix_path], true).result().await;
    assert_eq!(vec![nix_path], r.unwrap());
}

#[tokio::test]
async fn test_has_substitutes() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    assert!(store
        .has_substitutes(find_nix_in_store())
        .result()
        .await
        .unwrap());
}

#[tokio::test]
async fn test_query_substitutable_paths() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let nix_path = find_nix_in_store();
    let (stderrs, r) = store.query_substitutable_paths(&[&nix_path]).split().await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(vec![nix_path], r.unwrap());
}

#[tokio::test]
async fn test_query_pathinfo_none() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store.query_pathinfo(INVALID_STORE_PATH).split().await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);
    assert_eq!(None, r.unwrap());
}
#[tokio::test]
async fn test_query_pathinfo_some() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store.query_pathinfo(create_known_test_file()).split().await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);

    // We can't check the timestamp, so we have to compare the other fields one-by-one.
    let pi = r.expect("Error").expect("No PathInfo");
    assert_eq!(pi.deriver, None);
    assert_eq!(pi.references, Vec::<String>::new());
    assert_eq!(
        pi.nar_hash,
        "cb8becf17ebe664fecd6769b0384ce1a70830a1ac1f434e227c01ed774f91059".to_string()
    );
    assert_eq!(pi.nar_size, 416);
    assert_eq!(pi.ultimate, false);
    assert_eq!(pi.signatures, Vec::<String>::new());
    assert_eq!(
        pi.ca,
        Some("text:sha256:0h9xd0y2mzqnc73x9xnkkkqgi7rvya2b7ksdd0zdczjqsvhf4cpl".to_string())
    );
}

#[tokio::test]
async fn test_add_to_store() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let (stderrs, r) = store
        .add_to_store(
            "test_AddToStore",
            "fixed:r:sha256",
            Vec::<String>::new(),
            false,
            // $ echo -n "DaemonStore::add_to_store()" > test_AddToStore
            // $ nix-store --dump (nix-store --add test_AddToStore) | xxd -i
            Builder::new()
                .read(&[
                    0x0d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6e, 0x69, 0x78, 0x2d, 0x61,
                    0x72, 0x63, 0x68, 0x69, 0x76, 0x65, 0x2d, 0x31, 0x00, 0x00, 0x00, 0x01, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x74, 0x79, 0x70, 0x65,
                    0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x72,
                    0x65, 0x67, 0x75, 0x6c, 0x61, 0x72, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x63, 0x6f, 0x6e, 0x74, 0x65, 0x6e, 0x74, 0x73, 0x1b, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x44, 0x61, 0x65, 0x6d, 0x6f, 0x6e, 0x53, 0x74,
                    0x6f, 0x72, 0x65, 0x3a, 0x3a, 0x61, 0x64, 0x64, 0x5f, 0x74, 0x6f, 0x5f, 0x73,
                    0x74, 0x6f, 0x72, 0x65, 0x28, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x29, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00,
                ])
                .build(),
        )
        .split()
        .await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);

    let (name, pi) = r.expect("Progress");
    assert_eq!(
        concat!(
            "/nix/store/",
            "rplkfskrgxcfm49953si4jbinw9fg8sm",
            "-test_AddToStore"
        )
        .to_string(),
        name
    );
    assert_eq!(pi.deriver, None);
    assert_eq!(pi.references, Vec::<String>::new());
    assert_eq!(
        pi.nar_hash,
        "3c126cf4c0fec8c85cf9791ccdaf670877f9f9faf46b5d1991523d509b341d9e".to_string()
    );
    assert_eq!(pi.nar_size, 144);
    assert_eq!(pi.ultimate, false);
    assert_eq!(pi.signatures, Vec::<String>::new());
    assert_eq!(
        pi.ca,
        Some("fixed:r:sha256:17hx6jdm0gajj4cmsszlzbwzjxq8cypws73rz5fcij7yq3s6q4iw".to_string())
    );
}

#[tokio::test]
async fn test_nar_from_path() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");

    // Create a known derivation.
    let drv_path = create_known_test_file();

    println!("drv_path: {}", drv_path);
    // Get the nar.
    let (stderrs, r) = store.nar_from_path(&drv_path).split().await;
    assert_eq!(Vec::<Stderr>::new(), stderrs);

    println!("DONE");
    // Check the nar.
    // let nar = r.expect("Progress");
    // assert_eq!(144, nar.len());
}

#[tokio::test]
async fn test_query_missing_valid() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");
    let missing = store
        .query_missing(&[create_known_test_file()][..])
        .result()
        .await
        .expect("Progress");
    assert_eq!(
        missing,
        nix_daemon::Missing {
            will_build: Vec::new(),
            will_substitute: Vec::new(),
            unknown: Vec::new(),
            download_size: 0,
            nar_size: 0,
        }
    );
}

#[tokio::test]
async fn test_build() {
    init_logging();
    let mut store = DaemonStore::builder()
        .connect_unix("/nix/var/nix/daemon-socket/socket")
        .await
        .expect("Couldn't connect to daemon");

    // Disable substituters, else this test makes a network request.
    store
        .set_options(ClientSettings {
            use_substitutes: false,
            ..Default::default()
        })
        .result()
        .await
        .expect("SetOptions failed");

    // Random string our built derivation should output. This test expects to
    // actually build something, so it needs a derivation with a known output,
    // which is incredibly unlikely to already exist in the user's store.
    let cookie = {
        use rand::distributions::{Alphanumeric, DistString};
        Alphanumeric.sample_string(&mut rand::thread_rng(), 16)
    };

    // Instantiate a derivation.
    let mut nix_instantiate = std::process::Command::new("nix-instantiate")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Couldn't spawn nix-instantiate");
    std::thread::spawn({
        let mut stdin = nix_instantiate.stdin.take().unwrap();
        let input = format!(
            "derivation {{
                    name = \"test_build_paths_{}\";
                    builder = \"/bin/sh\";
                    args = [ \"-c\" \"echo -n $name > $out\" ];
                    system = builtins.currentSystem;
                }}",
            cookie,
        );
        move || stdin.write_all(input.as_bytes())
    });
    let nix_instantiate_output = nix_instantiate
        .wait_with_output()
        .expect("nix-instantiate failed");
    let drv_path_ = String::from_utf8(nix_instantiate_output.stdout).unwrap();
    let drv_path = drv_path_.trim();
    let drv_output = format!("{}!out", drv_path);

    // Double check that it's not already built.
    let missing = store
        .query_missing(&[&drv_output][..])
        .result()
        .await
        .expect("QueryMissing failed");
    assert_eq!(
        nix_daemon::Missing {
            will_build: vec![drv_path.to_string()],
            will_substitute: vec![],
            unknown: vec![],
            download_size: 0,
            nar_size: 0,
        },
        missing
    );

    // Build it!
    store
        .build_paths(&[&drv_output][..], BuildMode::Normal)
        .result()
        .await
        .expect("BuildPaths failed");

    // Check that BuildPathsWithResults decodes properly.
    let build_results = store
        .build_paths_with_results(&[&drv_output][..], BuildMode::Normal)
        .result()
        .await
        .expect("BuildPathsWithResults Progress");
    assert_eq!(
        HashMap::from([(
            drv_output.clone(),
            nix_daemon::BuildResult {
                status: nix_daemon::BuildResultStatus::AlreadyValid,
                error_msg: String::new(),
                ..build_results[&drv_output].clone()
            }
        )]),
        build_results
    );

    // Check the output.
    let nix_store_query = std::process::Command::new("nix-store")
        .arg("--query")
        .arg(&drv_path)
        .output()
        .expect("Couldn't create known test derivation");
    let out_path_ = String::from_utf8(nix_store_query.stdout).unwrap();
    let out_path = out_path_.trim();

    let content =
        std::fs::read_to_string(std::path::PathBuf::from(out_path)).expect("Couldn't read output");
    assert_eq!(format!("test_build_paths_{}", cookie), content);

    // Add a temporary root.
    store
        .add_temp_root(&drv_path)
        .result()
        .await
        .expect("AddTempRoot failed");

    // Add an indirect root, check that it's registered properly.
    let root_path = std::env::temp_dir().join("test_build_result");
    let root_path_str = root_path.to_str().unwrap();
    if root_path.try_exists().expect("Couldn't check stale root") {
        warn!(?root_path, "Removing stale root");
        std::fs::remove_file(&root_path).expect("Couldn't remove stale root");
    }
    std::os::unix::fs::symlink(&out_path, &root_path).expect("Couldn't symlink");

    store
        .add_indirect_root(&root_path_str)
        .result()
        .await
        .expect("AddIndirectRoot failed");
    let roots = store.find_roots().result().await.expect("FindRoots failed");
    assert_eq!(roots[root_path_str], out_path);

    // Delete the indirect root, watch it get deregistered.
    std::fs::remove_file(&root_path).expect("Couldn't remove root");
    let roots2 = store
        .find_roots()
        .result()
        .await
        .expect("FindRoots 2 failed");
    assert_eq!(
        false,
        roots2.contains_key(root_path_str),
        "Deleted root persisted?"
    );

    // Try QueryValidDerivers.
    let valid_derivers = store
        .query_valid_derivers(&out_path)
        .result()
        .await
        .expect("QueryValidDerivers failed");
    assert_eq!(vec![drv_path], valid_derivers);

    // Try QueryDerivationOutputMap.
    let outputs = store
        .query_derivation_output_map(&drv_path)
        .result()
        .await
        .expect("QueryDerivationOutputMap failed");
    assert_eq!(
        HashMap::from([("out".to_string(), out_path.to_string())]),
        outputs
    );
}
