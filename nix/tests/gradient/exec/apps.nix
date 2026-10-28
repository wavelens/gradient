/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs }: let
  sleep = "${pkgs.coreutils}/bin/sleep";

  ticks = name: firstAttempt: pkgs.writeShellScript name ''
    count=${toString firstAttempt}
    [ "$GRADIENT_ATTEMPT" = 1 ] || count=5
    for ((i = 1; i <= count; i++)); do
      echo "tick $i"
      ${sleep} 1
    done
  '';
in {
  fetch = pkgs.writeShellScript "exec-fetch" ''
    echo "job=$GRADIENT_JOB attempt=$GRADIENT_ATTEMPT"
    exec 3<>/dev/tcp/web.exec.test/8080
    printf 'GET / HTTP/1.0\r\nHost: web.exec.test\r\n\r\n' >&3
    while IFS= read -r line <&3; do
      echo "$line"
    done
  '';

  fail = pkgs.writeShellScript "exec-fail" ''
    echo "attempt $GRADIENT_ATTEMPT exits with 3"
    exit 3
  '';

  silent = pkgs.writeShellScript "exec-silent" ''
    echo "going quiet"
    ${sleep} 120
  '';

  interactive = pkgs.writeShellScript "exec-interactive" ''
    exec ${pkgs.python3}/bin/python3 -m http.server 8080 --bind 127.0.0.1 --directory /work
  '';

  terminal = pkgs.writeShellScript "exec-terminal" ''
    exec ${pkgs.bashInteractive}/bin/bash -i
  '';

  secrets = pkgs.writeShellScript "exec-secrets" ''
    echo "env $GRADIENT_SECRET_API_TOKEN"
    printf %s "$GRADIENT_SECRET_WANDB_TOKEN" | ${pkgs.coreutils}/bin/base64
    echo "file $(${pkgs.coreutils}/bin/cat /run/gradient/secrets/lab-key)"
    echo "arg $1"
  '';

  clash = pkgs.writeShellScript "exec-clash" ''
    echo "clash started"
  '';

  abort = ticks "exec-abort" 600;
  long-stop = ticks "exec-long-stop" 300;
  long-keep = ticks "exec-long-keep" 150;
  lost = ticks "exec-lost" 300;
}
