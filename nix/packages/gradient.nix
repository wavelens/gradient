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

    nativeBuildInputs = [
      installShellFiles
      pkg-config
      (lib.getDev gradient-nix)
      (lib.getDev glibc)
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
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
  pname = "gradient";
  version = "1.3.0";
  separateDebugInfo = true;

  nativeCheckInputs = [ git ];
  preCheck = ''
    ln -s ${testStore} ./test-store
  '';

  meta = {
    description = "Nix Continuous Integration System Backend";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-server";
  };
})
