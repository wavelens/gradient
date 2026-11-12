/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib }:
let
  packageProgram = import ../../../backend/gradient-eval/src/run/package.nix;
  devShellProgram = import ../../../backend/gradient-eval/src/run/devshell.nix;
  throws = e: !(builtins.tryEval (builtins.deepSeq e e)).success;
  derivationsIn = program: builtins.length (builtins.attrNames (builtins.getContext program));

  shell = pkgs.mkShell {
    packages = [ pkgs.bashInteractive ];
    GREETING = "hello from the shell";
    shellHook = "echo shell ready";
  };
  shellProgram = devShellProgram shell;
  plainShellProgram = devShellProgram (pkgs.mkShell { });
in
assert packageProgram pkgs.hello == "${pkgs.hello}/bin/hello";
assert packageProgram (pkgs.hello // { meta.mainProgram = "hi"; }) == "${pkgs.hello}/bin/hi";
assert packageProgram pkgs.curl == "${pkgs.curl.bin}/bin/curl";
assert derivationsIn (packageProgram pkgs.curl) == 1;
assert throws (packageProgram { });
assert throws (packageProgram "hello");
assert lib.hasSuffix "/bin/gradient-devshell" shellProgram;
assert derivationsIn shellProgram == 1;
assert throws (devShellProgram (derivation { name = "raw"; system = pkgs.stdenv.hostPlatform.system; builder = "/bin/sh"; }));
pkgs.runCommand "run-wrapping-check" { } ''
  launcher=${lib.removeSuffix "/bin/gradient-devshell" shellProgram}
  test -x "$launcher/bin/gradient-devshell"
  grep -q 'bash-interactive-.*--rcfile' "$launcher/bin/gradient-devshell"
  grep -q 'GREETING=hello' "$launcher/rc"
  grep -q 'echo shell ready' "$launcher/rc"
  grep -q '^exec bash --rcfile' ${plainShellProgram}
  touch $out
''
