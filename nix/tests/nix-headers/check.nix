/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs }:
let
  checkHeader = pkgs.writeShellScript "check-header" ''
    include() { echo "#include \"$1\""; }
    include "$1" | ${pkgs.gcc}/bin/g++ -std=c++23 -fsyntax-only -x c++ - 2>/dev/null || exit 0
    include "$1" | "$CXX" -std=c++23 -fsyntax-only -Wno-unused-command-line-argument -x c++ - || { echo "$1 compiles with libstdc++ but not with libc++"; exit 1; }
  '';
in
pkgs.libcxxStdenv.mkDerivation {
  name = "gradient-nix-headers-libcxx";
  dontUnpack = true;
  buildInputs = [ pkgs.gradient-nix.dev ];

  buildPhase = ''
    runHook preBuild
    cd ${pkgs.gradient-nix.dev}/include
    find -L . -name '*.h' -o -name '*.hh' | sort | xargs -P "$NIX_BUILD_CORES" -n 1 ${checkHeader}
    runHook postBuild
  '';

  installPhase = "touch $out";
}
