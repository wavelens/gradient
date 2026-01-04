/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, git
, installShellFiles
, nix
, nixVersions
, openssl
, pkg-config
, rustPlatform
, stdenv
}: let
  ignoredPaths = [ ".github" "target" ];
in rustPlatform.buildRustPackage {
  pname = "gradient-cli";
  version = "0.4.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) ignoredPaths);
    src = lib.cleanSource ../../cli;
  };

  nativeBuildInputs = [
    installShellFiles
    pkg-config
  ];

  buildInputs = [
    git
    nix
    nixVersions.latest
    openssl
    pkg-config
  ];

  cargoLock.lockFile = ../../cli/Cargo.lock;

  NIX_INCLUDE_PATH = "${lib.getDev nix}/include";

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
    platforms = lib.platforms.unix;
    mainProgram = "gradient";
  };
}
