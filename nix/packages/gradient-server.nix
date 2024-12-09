/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

{ lib
, git
, installShellFiles
, nix
, nixVersions
, openssl
, pkg-config
, rustPlatform
}: let
  ignoredPaths = [ ".github" "target" ];
in rustPlatform.buildRustPackage {
  pname = "gradient-server";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) ignoredPaths);
    src = lib.cleanSource ../../backend;
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

  cargoLock = {
    lockFile = ../../backend/Cargo.lock;
    outputHashes."nix-daemon-0.1.2" = "sha256-VOvtYN1+QwHmLqoGS5N5e9Wrtba+RY9vSPuBw/7hu9o=";
    allowBuiltinFetchGit = true;
  };

  NIX_INCLUDE_PATH = "${lib.getDev nix}/include";

  meta = {
    description = "Nix Continuous Integration System Backend";
    homepage = "https://wavelens.io";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-server";
  };
}
