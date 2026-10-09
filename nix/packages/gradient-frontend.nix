/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, fetchPnpmDeps
, gradient-logo
, nodejs
, pnpm
, pnpmConfigHook
, stdenv
}: stdenv.mkDerivation rec {
  pname = "gradient-frontend";
  version = "2.0.0-rc.1";
  __structuredAttrs = true;

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) [".github" "target" "node_modules" "dist" ".angular"]);
    src = lib.cleanSource ../../frontend;
  };

  pnpmDeps = fetchPnpmDeps {
    inherit pnpm pname version src;
    fetcherVersion = 4;
    hash = "sha256-1JNu6GIWdALMi3ngGBL/m+0JH8/MeoL6jwX+gtwzxUk=";
  };

  nativeBuildInputs = [
    nodejs
    pnpm
    pnpmConfigHook
  ];

  # The prebuilt `sass-embedded` Dart binary cannot run in the sandbox.
  # The pure-JS compiler is replacing it.
  env.NG_BUILD_SASS_EMBEDDED = "0";

  postPatch = ''
    install -Dm444 ${gradient-logo}/logo-transparent.svg public/images/logo.svg
  '';

  buildPhase = ''
    runHook preBuild

    pnpm run build

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out/share/gradient-frontend
    cp -r dist/gradient-frontend/browser/* $out/share/gradient-frontend/
    install -Dm444 dist/gradient-frontend/3rdpartylicenses.txt \
      $out/share/doc/gradient-frontend/3rdpartylicenses.txt

    runHook postInstall
  '';

  meta = {
    description = "Nix-CI for Teams (frontend)";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
  };
}
