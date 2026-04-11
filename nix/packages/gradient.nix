/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib
, git
, glibc
, installShellFiles
, llvmPackages
, nixVersions
, openssl
, pkg-config
, rustPlatform
, zstd
}:
let
  nixVersion = nixVersions.nix_2_34;
  ignoredPaths = [ ".github" "target" ];
in rustPlatform.buildRustPackage {
  pname = "gradient";
  version = "1.0.0";

  src = lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) ignoredPaths);
    src = lib.cleanSource ../../backend;
  };

  nativeBuildInputs = [
    installShellFiles
    pkg-config
    (lib.getDev nixVersion)
    (lib.getDev glibc)
  ];

  buildInputs = [
    git
    nixVersion
    openssl
    zstd
  ];

  cargoLock = {
    lockFile = ../../backend/Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
  BINDGEN_EXTRA_CLANG_ARGS = "--sysroot=${glibc.dev}";

  meta = {
    description = "Nix Continuous Integration System Backend";
    homepage = "https://github.com/wavelens/gradient";
    license = lib.licenses.agpl3Only;
    platforms = lib.platforms.unix;
    mainProgram = "gradient-server";
  };
}
