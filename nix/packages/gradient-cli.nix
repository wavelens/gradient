/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, craneLib
, git
, installShellFiles
, nixVersions
, openssl
, pkg-config
, stdenv
}:
let
  nixVersion = nixVersions.nix_2_34;

  src = craneLib.cleanCargoSource ../../cli;

  commonArgs = {
    inherit src;
    strictDeps = true;

    nativeBuildInputs = [
      installShellFiles
      pkg-config
    ];

    buildInputs = [
      git
      nixVersion
      openssl
    ];
  };

  # Cached dependency layer — only rebuilt when Cargo.lock changes
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
