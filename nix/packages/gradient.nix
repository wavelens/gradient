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

  # nix-bindings has readme in workspace crate; include md files alongside cargo sources
  depsSrc = lib.fileset.toSource {
    root = unfilteredRoot;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "md") unfilteredRoot)
    ];
  };

  # Final build also needs .nix files and include_str! assets like the migration baseline .sql
  src = lib.fileset.toSource {
    root = unfilteredRoot;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "md") unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "nix") unfilteredRoot)
      (lib.fileset.fileFilter (file: file.hasExt "sql") unfilteredRoot)
      # The plan gate's unit tests read these EXPLAIN fixtures at run time, so
      # they have to survive the cargo-source filter that drops every non-source.
      (lib.fileset.fileFilter (file: file.hasExt "json") ../../backend/gradient-db/tests)
    ];
  };

  # strip readme from all crate checkouts
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

    # A sandbox starts with no incremental cache to reuse, so the bookkeeping
    # is pure overhead and it fattens the target dir crane packs between layers.
    CARGO_INCREMENTAL = "0";

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

    LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
    BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${glibc.dev}";
  };

  # crane's default dummy source. Provide a minimal stub that compiles
  dummyrs = pkgs.writeText "dummy.rs" ''
    #![allow(clippy::all)]
    #![allow(dead_code)]
    pub fn main() {}
  '';

  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
    src = depsSrc;
    inherit dummyrs;
  });

  # The suite builds under `[profile.test]`, so it cannot share the release
  # layer. `[profile.dev.package."*"]` keeps the dependencies optimised, which
  # is what stops an unoptimised argon2 from outlasting the compile it saves.
  testArtifacts = craneLib.buildDepsOnly (commonArgs // {
    src = depsSrc;
    pname = "gradient-server-test";
    inherit dummyrs;
    CARGO_PROFILE = "test";
  });
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
  pname = "gradient";
  version = "1.3.0";
  separateDebugInfo = true;

  # `separateDebugInfo` exports `NIX_RUSTFLAGS=-g -C strip=none` for the whole
  # derivation. Keep that on the shipped binary and off the ~105 test targets:
  # the suite is its own check, so it neither carries full DWARF nor blocks
  # everything that only needs the binary.
  doCheck = false;

  # Reuses cargoArtifacts so clippy only recompiles workspace crates.
  passthru.clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
  });

  # The SQL plan gate the cache VM test runs. Behind `required-features`, so a
  # default build never compiles it and it never lands in this package.
  passthru.sqlGate = craneLib.buildPackage (commonArgs // {
    inherit cargoArtifacts;
    pname = "gradient-sql-gate";
    version = "1.3.0";
    cargoExtraArgs = "--features sql-gate --bin gradient-sql-gate";
    doCheck = false;
  });

  passthru.tests = craneLib.cargoNextest (commonArgs // {
    cargoArtifacts = testArtifacts;
    version = "1.3.0";
    CARGO_PROFILE = "test";
    cargoExtraArgs = "--locked";

    nativeCheckInputs = [ git ];
    preCheck = ''
      ln -s ${testStore} ./test-store
    '';
  });

  # nextest cannot run doc tests, so they get their own check rather than
  # silently dropping out of the suite.
  passthru.docTests = craneLib.cargoDocTest (commonArgs // {
    cargoArtifacts = testArtifacts;
    version = "1.3.0";
    CARGO_PROFILE = "test";
  });

  meta = {
    description = "Nix Continuous Integration System Backend";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-server";
  };
})
