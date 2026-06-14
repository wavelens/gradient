/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, craneLib
, git
, installShellFiles
, gradient-nix
, openssl
, pkg-config
, stdenv
, cargoFeatures ? [ ]
}:
let
  src = craneLib.cleanCargoSource ../../cli;

  # harmonia (git dep) ships crates whose Cargo.toml points at a README.md that
  # isn't in the crate dir; strip the readme key so vendoring doesn't choke.
  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;
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

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ];

    buildInputs = [
      git
      gradient-nix
      openssl
    ];
  };

  # Cached dependency layer - only rebuilt when Cargo.lock or features change
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
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
