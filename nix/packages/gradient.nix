/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

{ lib
, installShellFiles
, nix
, openssl
, pkg-config
, nixVersions
, rustPlatform
}: let
  ignoredPaths = [ ".github" "target" ];
in rustPlatform.buildRustPackage {
  pname = "gradient";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) ignoredPaths);
    src = lib.cleanSource ../../backend;
  };

  nativeBuildInputs = [
    pkg-config
    installShellFiles
  ];

  buildInputs = [
    nix
    openssl
    pkg-config
    nixVersions.latest
  ];

  cargoLock = {
    lockFile = ../../backend/Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  NIX_INCLUDE_PATH = "${lib.getDev nix}/include";

  doCheck = false;

  meta = {
    description = "Nix Build Server";
    homepage = "https://wavelens.io";
    platforms = lib.platforms.linux;
    mainProgram = "backend";
  };
}
