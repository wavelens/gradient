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

  # crane's default dummy trips the workspace lints; this one compiles under them.
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

    CARGO_INCREMENTAL = "0";

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ];

    buildInputs = [
      git
      gradient-nix
      openssl
    ];
  } // (lib.optionalAttrs withEval {
    LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
  } // lib.optionalAttrs stdenv.hostPlatform.isLinux {
    BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${stdenv.cc.libc.dev}";
  });

  # The cli workspace sits in a subdirectory because the `eval` feature pulls
  # gradient-eval from backend/. `mkDummySrc` keeps the source's store name, so
  # `sourceRoot` resolves in the dummy tree too, but it only carries over a
  # Cargo.lock sitting at the source root: this one has to be put back by hand.
  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
    inherit dummyrs;
    extraDummyScript = ''
      cp ${cliSrc + "/Cargo.lock"} $out/cli/Cargo.lock
    '';
  });
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
  pname = "gradient-cli";
  version = "1.3.0";
  separateDebugInfo = true;

  # Same split as the server: the binary keeps the debug output, the suite runs
  # as its own check instead of inside the package.
  doCheck = false;

  # Reuses cargoArtifacts so clippy only recompiles the workspace crates.
  passthru.clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
  });

  passthru.tests = craneLib.cargoNextest (commonArgs // {
    inherit cargoArtifacts;
    version = "1.3.0";
  });

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
