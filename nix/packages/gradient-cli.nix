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
, stdenv
, cargoFeatures ? [ ]
}:
let
  # The `eval` feature path-depends on backend/gradient-eval (the shared Nix
  # evaluator) and thus on libnix; only then do we pull the Nix dev toolchain.
  withEval = builtins.elem "eval" cargoFeatures;

  repoRoot = ../..;

  # The CLI is its own cargo workspace, but `gradient-eval` lives under backend/,
  # so the source tree must carry both crates. cargo builds from the cli subdir
  # (sourceRoot below) and resolves the `../backend/gradient-eval` path dep.
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

  # harmonia/nix-bindings (git deps) ship crates whose Cargo.toml points at a
  # README.md outside the crate dir; strip the readme key so vendoring works.
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

  # Crane has no easy way to set Cargo features, this sets them manually via cargoExtraArgs.
  # It has `--locked` hard coded since that is the default of Crane.
  cargoExtraArgs = lib.concatStringsSep " " (
    [ "--locked" ]
    ++ lib.optional (cargoFeatures != [ ]) "--features ${lib.concatStringsSep "," cargoFeatures}"
  );

  commonArgs = {
    inherit src cargoExtraArgs cargoVendorDir;
    strictDeps = true;
    sourceRoot = "${src.name}/cli";
    cargoToml = cliSrc + "/Cargo.toml";

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ] ++ lib.optionals withEval [
      (lib.getDev gradient-nix)
      (lib.getDev glibc)
    ];

    buildInputs = [
      git
      gradient-nix
      openssl
    ];
  } // lib.optionalAttrs withEval {
    LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
    BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${glibc.dev}";
  };
in
# Deps and crate build in one pass: the `eval` feature pulls gradient-eval from
# backend/, so the cli workspace lives in a subdirectory of the source tree, and
# crane's split deps layer assumes the workspace is at the source root.
craneLib.buildPackage (commonArgs // {
  cargoArtifacts = null;
  pname = "gradient-cli";
  version = "1.2.0";
  separateDebugInfo = true;

  postInstall = lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
    installShellCompletion --cmd gradient \
      --bash <($out/bin/gradient completion bash) \
      --fish <($out/bin/gradient completion fish) \
      --zsh <($out/bin/gradient completion zsh)
  '';

  meta = {
    description = "Gradient cli tool";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    mainProgram = "gradient";
  };
})
