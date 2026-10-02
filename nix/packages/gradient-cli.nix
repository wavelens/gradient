/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, craneLib
, git
, installShellFiles
, llvmPackages
, gradient-nix
, openssl
, pkg-config
, stdenv
, writeText
, cargoFeatures ? [ ]
}:
let
  withEval = builtins.elem "eval" cargoFeatures;

  repoRoot = ../..;

  mdFiles = dir: lib.fileset.fileFilter (f: f.hasExt "md") dir;
  cliSrc = repoRoot + "/cli";
  evalSrc = repoRoot + "/backend/gradient-eval";
  src = lib.fileset.toSource {
    root = repoRoot;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources cliSrc)
      (mdFiles cliSrc)
      (craneLib.fileset.commonCargoSources evalSrc)
      (mdFiles evalSrc)
    ];
  };

  # Crates of the harmonia and nix-bindings git deps are pointing `readme` outside the crate dir.
  # Vendoring is failing until that key is stripped.
  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;
    cargoLock = cliSrc + "/Cargo.lock";
    overrideVendorGitCheckout = _ps: drv:
      drv.overrideAttrs (old: {
        postPatch = (old.postPatch or "") + ''
          find . -name "Cargo.toml" | xargs sed -i '/^readme\s*=/d'
        '';
      });
  };

  cargoExtraArgs = lib.concatStringsSep " " (
    [ "--locked" ]
    ++ lib.optional (cargoFeatures != [ ]) "--features ${lib.concatStringsSep "," cargoFeatures}"
  );

  dummyrs = writeText "dummy.rs" ''
    #![allow(clippy::all)]
    #![allow(dead_code)]
    pub fn main() {}
  '';

  commonArgs = {
    inherit src cargoExtraArgs cargoVendorDir;
    strictDeps = true;
    sourceRoot = "${src.name}/cli";
    cargoToml = cliSrc + "/Cargo.toml";
    __structuredAttrs = true;

    env = {
      CARGO_INCREMENTAL = "0";
    } // lib.optionalAttrs withEval {
      LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
    } // lib.optionalAttrs stdenv.hostPlatform.isLinux {
      BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${stdenv.cc.libc.dev}";
    };

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ];

    buildInputs = [
      openssl
    ] ++ lib.optionals (!stdenv.hostPlatform.isStatic) [
      git
      gradient-nix
    ];
  };

  # `mkDummySrc` is carrying over only a Cargo.lock at the source root.
  # The cli lock is living in a subdirectory and must be copied back by hand.
  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
    inherit dummyrs;
    extraDummyScript = ''
      cp ${cliSrc + "/Cargo.lock"} $out/cli/Cargo.lock
    '';
  });
in
craneLib.buildPackage (commonArgs // rec {
  inherit cargoArtifacts;
  pname = "gradient-cli";
  version = "1.4.1";
  separateDebugInfo = true;

  doCheck = false;

  passthru.clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
  });

  passthru.tests = craneLib.cargoNextest (commonArgs // {
    inherit cargoArtifacts version;
  });

  postInstall = lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
    installShellCompletion --cmd gradient \
      --bash <($out/bin/gradient completion bash) \
      --fish <($out/bin/gradient completion fish) \
      --zsh <($out/bin/gradient completion zsh)
  '' + lib.optionalString stdenv.hostPlatform.isStatic ''
    mkdir -p $out/nix-support
    echo "file binary-dist $out/bin/gradient" >> $out/nix-support/hydra-build-products
  '';

  meta = {
    description = "Gradient cli tool";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient";
  };
})
