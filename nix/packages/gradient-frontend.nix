/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, fetchPnpmDeps
, nodejs
, pnpm
, pnpmConfigHook
, stdenv
}:

stdenv.mkDerivation rec {
  pname = "gradient-frontend";
  version = "0.5.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) [".github" "target" "node_modules" "dist" ".angular"]);
    src = lib.cleanSource ../../frontend;
  };

  pnpmDeps = fetchPnpmDeps {
    inherit pname version src;
    fetcherVersion = 3;
    hash = "sha256-nA5UPAls9Q7NKb47C+NrGu+skuO+tRnitgNzQ+t3+Gw=";
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
