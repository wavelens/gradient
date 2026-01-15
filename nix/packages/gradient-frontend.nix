/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, buildNpmPackage
, nodejs
}:

buildNpmPackage rec {
  pname = "gradient-frontend";
  version = "0.5.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) [".github" "target" "node_modules" "dist" ".angular"]);
    src = lib.cleanSource ../../frontend;
  };

  npmDepsHash = lib.fakeHash;  # Run once to get the actual hash, then replace

  nativeBuildInputs = [ nodejs ];

  # Skip npm audit during build
  npmBuildScript = "build";

  # Build configuration
  buildPhase = ''
    runHook preBuild

    npm run build -- --configuration production

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
