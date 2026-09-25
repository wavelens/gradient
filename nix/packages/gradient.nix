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
      # Unit tests read these JSON fixtures at run time, so they have to
      # survive the cargo-source filter that drops every non-source.
      (lib.fileset.fileFilter (file: file.hasExt "json") ../../backend/gradient-db/tests)
      (lib.fileset.fileFilter (file: file.hasExt "json") ../../backend/gradient-daemon/tests)
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

    # crane leads with `cargo check --all-targets` to cache check artifacts.
    # Only clippy reads those and clippy reads the release layer, so here that
    # pass is six minutes for nothing. The check phase's `cargo test --no-run`
    # still brings in the dev-dependencies.
    buildPhaseCargoCommand = "cargoWithProfile build $cargoExtraArgs";
  });
in
craneLib.buildPackage (commonArgs // rec {
  inherit cargoArtifacts;
  pname = "gradient";
  version = "1.4.0";
  separateDebugInfo = true;

  # `separateDebugInfo` exports `NIX_RUSTFLAGS=-g -C strip=none` for the whole
  # derivation. Keep that on the shipped binary and off the ~105 test targets:
  # the suite is its own check, so it neither carries full DWARF nor blocks
  # everything that only needs the binary.
  doCheck = false;

  # The SQL plan gate and the mock nix-daemon the VM tests run come out of this
  # same cargo invocation instead of a second one over the whole workspace:
  # both features only flip optional dependencies, and `required-features`
  # keeps the bins out of a default build. Their own outputs keep them out of
  # the server's closure.
  outputs = [ "out" "gate" "daemon" ];
  cargoExtraArgs = "--locked --features sql-gate,gradient-daemon/mock";

  postInstall = ''
    mkdir -p $gate/bin $daemon/bin
    mv $out/bin/gradient-sql-gate $gate/bin/
    mv $out/bin/gradient-daemon $daemon/bin/
  '';

  # Reuses cargoArtifacts so clippy only recompiles workspace crates.
  passthru.clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
  });

  passthru.tests = craneLib.cargoNextest (commonArgs // {
    inherit version;
    cargoArtifacts = testArtifacts;
    CARGO_PROFILE = "test";

    nativeCheckInputs = [ git ];
    preCheck = ''
      ln -s ${testStore} ./test-store
    '';

    # nextest runs no doc tests. Here the workspace is already compiled and
    # they cost 24 s; a derivation of their own spent six minutes rebuilding
    # it to run the two that exist.
    postCheck = ''
      cargoWithProfile test --doc $cargoExtraArgs
    '';
  });

  meta = {
    description = "Nix Continuous Integration System Backend";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-server";
  };
})
