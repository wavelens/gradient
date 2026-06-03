/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, path
, callPackage
, fetchPnpmDeps
, nodejs
, pnpmConfigHook
, stdenv
}: let
  # pin pnpm version to avoid hash mismatches with differing pnpm versions in nixos stable
  pnpm = callPackage (path + "/pkgs/development/tools/pnpm/generic.nix") {
    version = "11.1.1";
    hash = "sha256-BbKC0GMyKVxzbwsgyL3xhTJb8bymgske2BFUo8aFHMA=";
  };
in stdenv.mkDerivation rec {
  pname = "gradient-frontend";
  version = "1.2.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) [".github" "target" "node_modules" "dist" ".angular"]);
    src = lib.cleanSource ../../frontend;
  };

  pnpmDeps = fetchPnpmDeps {
    inherit pnpm pname version src;
    fetcherVersion = 3;
    hash = "sha256-AI3jv/moD6JY5yhlFXej4OIpodae29R3GjGQdCIj+tw=";
  };

  nativeBuildInputs = [
    nodejs
    pnpm
    pnpmConfigHook
  ];

  buildPhase = ''
    runHook preBuild

    pnpm run build

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out/share/gradient-frontend
    cp -r dist/gradient-frontend/browser/* $out/share/gradient-frontend/

    runHook postInstall
  '';

  meta = {
    description = "Nix Continuous Integration System Frontend";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
  };
}
