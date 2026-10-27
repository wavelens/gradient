/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, cacert
, craneLib
, git
, glibc
, installShellFiles
, llvmPackages
, gradient-nix
, libgit2
, libssh2
, openssl
, pkg-config
, pkgs
, postgresql_18
, postgresqlTestHook
, sqlite
, xz
, zlib
, zstd
}:
let
  testStore = import ../scripts/store.nix { inherit pkgs; };

  unfilteredRoot = ../../backend;

  depsSrc = lib.fileset.toSource {
    root = unfilteredRoot;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "md") unfilteredRoot)
    ];
  };

  src = lib.fileset.toSource {
    root = unfilteredRoot;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "md") unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "nix") unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "sql") unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "json") ../../backend/gradient-db/tests)
      (lib.fileset.fileFilter (file: file.hasExt "json") ../../backend/gradient-daemon/tests)
      ../../backend/gradient-types/options.json
      ../../backend/gradient-worker/options.json
      ../../backend/gradient-wire/schema
      ../../backend/gradient-ssh/testdata
    ];
  };

  cargoVendorDir = craneLib.vendorCargoDeps {
    src = depsSrc;
    overrideVendorGitCheckout = _ps: drv:
      drv.overrideAttrs (old: {
        postPatch = (old.postPatch or "") + ''
          find . -name "Cargo.toml" | xargs sed -i '/^readme\s*=/d'
        '';
      });
  };

  commonArgs = {
    inherit src cargoVendorDir;
    strictDeps = true;
    __structuredAttrs = true;

    env = {
      CARGO_INCREMENTAL = "0";
      LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
      BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${glibc.dev}";
      LIBGIT2_NO_VENDOR = "1";
      LIBSQLITE3_SYS_USE_PKG_CONFIG = "1";
      ZSTD_SYS_USE_PKG_CONFIG = "1";
    };

    cargoExtraArgs = "--locked --features gradient-daemon/mock";

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ];

    buildInputs = [
      git
      gradient-nix
      libgit2
      libssh2
      openssl
      sqlite
      xz
      zlib
      zstd
    ];
  };

  dummyrs = pkgs.writeText "dummy.rs" ''
    #![allow(clippy::all)]
    #![allow(dead_code)]
    fn main() {}
  '';

  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
    src = depsSrc;
    inherit dummyrs;
  });

  testArtifacts = craneLib.buildDepsOnly (commonArgs // {
    src = depsSrc;
    pname = "gradient-server-test";
    inherit dummyrs;
    CARGO_PROFILE = "test";

    # crane is starting with `cargo check --all-targets` to cache check artifacts.
    # Only clippy is reading those, and clippy is using the release layer.
    # That pass would cost six minutes for nothing here.
    buildPhaseCargoCommand = "cargoWithProfile build $cargoExtraArgs";
  });
in
craneLib.buildPackage (commonArgs // rec {
  inherit cargoArtifacts;
  pname = "gradient";
  version = "2.0.1";
  separateDebugInfo = true;

  # `separateDebugInfo` is exporting `NIX_RUSTFLAGS=-g -C strip=none` for the whole derivation.
  # The suite is its own check to keep full DWARF off the ~105 test targets.
  doCheck = false;

  outputs = [ "out" "gate" "daemon" "guest" ];
  cargoExtraArgs = "--locked --features sql-gate,api-spec,gradient-daemon/mock";

  postInstall = ''
    mkdir -p $gate/bin $daemon/bin $guest/bin
    mv $out/bin/gradient-sql-gate $gate/bin/
    mv $out/bin/gradient-api-spec $gate/bin/
    mv $out/bin/gradient-daemon $daemon/bin/
    mv $out/bin/gradient-guest-agent $guest/bin/
  '';

  passthru.clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
    doInstallCargoArtifacts = false;
  });

  passthru.tests = craneLib.cargoNextest (commonArgs // {
    inherit version;
    cargoArtifacts = testArtifacts;
    CARGO_PROFILE = "test";
    cargoNextestExtraArgs = "--no-fail-fast";
    doInstallCargoArtifacts = false;

    nativeCheckInputs = [ cacert git postgresql_18 postgresqlTestHook ];
    postgresqlTestUserOptions = "LOGIN CREATEDB";
    postgresqlExtraSettings = ''
      fsync = off
      max_connections = 500
    '';
    preCheck = ''
      ln -s ${testStore} ./test-store
    '';

    postCheck = ''
      cargoWithProfile test --doc $cargoExtraArgs
    '';
  });

  meta = {
    description = "Nix-CI for Teams (backend)";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-server";
  };
})
