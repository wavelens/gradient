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

  abort = ticks "exec-abort" 600;
  long-stop = ticks "exec-long-stop" 300;
  long-keep = ticks "exec-long-keep" 150;
  lost = ticks "exec-lost" 300;
}
