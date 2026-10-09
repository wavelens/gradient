/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, craneLib
, git
, glibc
, installShellFiles
, llvmPackages
, gradient-nix
, openssl
, pkg-config
, pkgs
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
    };

    cargoExtraArgs = "--locked --features gradient-daemon/mock";

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ];

    buildInputs = [
      git
      gradient-nix
      openssl
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
  version = "2.0.0-rc.1";
  separateDebugInfo = true;

  # `separateDebugInfo` is exporting `NIX_RUSTFLAGS=-g -C strip=none` for the whole derivation.
  # The suite is its own check to keep full DWARF off the ~105 test targets.
  doCheck = false;

  outputs = [ "out" "gate" "daemon" ];
  cargoExtraArgs = "--locked --features sql-gate,gradient-daemon/mock";

  postInstall = ''
    mkdir -p $gate/bin $daemon/bin
    mv $out/bin/gradient-sql-gate $gate/bin/
    mv $out/bin/gradient-daemon $daemon/bin/
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
    doInstallCargoArtifacts = false;

    nativeCheckInputs = [ git ];
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
