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

  manual = pkgs.writeShellScript "exec-manual" ''
    echo "started by hand"
  '';

  outputs = pkgs.writeShellScript "exec-outputs" ''
    set -euo pipefail
    gradient-phase epoch-1 <(echo state-1)
    ${pkgs.coreutils}/bin/head -c 3000000 /dev/urandom > /tmp/w.bin
    gradient-upload /tmp/w.bin model/w.bin
    echo changed >> README
    echo new > new.txt
    gradient build --no-link --background packages.x86_64-linux.hello
  '';

  pair = pkgs.writeShellScript "exec-pair" ''
    echo "master $1"
    echo "peers $GRADIENT_PEERS"
    if [ "$GRADIENT_NODE_INDEX" = 0 ]; then
      exec ${pkgs.socat}/bin/socat -u TCP-LISTEN:"$GRADIENT_MASTER_PORT",reuseaddr STDOUT
    fi
    for ((i = 0; i < 60; i++)); do
      echo "hello from node $GRADIENT_NODE_INDEX of $GRADIENT_NODE_COUNT" \
        | ${pkgs.socat}/bin/socat -u STDIN TCP:"$GRADIENT_MASTER_ADDR:$GRADIENT_MASTER_PORT" && exit 0
      ${sleep} 1
    done
    exit 1
  '';

  sized = pkgs.writeShellScript "exec-sized" ''
    echo "cpus $(${pkgs.coreutils}/bin/nproc)"
    while read -r key value _; do
      [ "$key" = MemTotal: ] && echo "memory_kb $value"
    done < /proc/meminfo
    echo "work_mb $(${pkgs.coreutils}/bin/df -m --output=size /work | ${pkgs.coreutils}/bin/tail -n 1)"
  '';

  toolarge = pkgs.writeShellScript "exec-toolarge" ''
    echo "a guest of 64 CPUs never boots in this test"
  '';

  hello = pkgs.writeText "exec-hello" "hello\n";

  trainer = pkgs.writeShellScript "exec-trainer" ''
    echo "$(${pkgs.coreutils}/bin/cat /etc/gradient-image)"
    echo "cwd $PWD"
  '';

  noagent = pkgs.writeShellScript "exec-noagent" "true";

  abort = ticks "exec-abort" 600;
  long-stop = ticks "exec-long-stop" 300;
  long-keep = ticks "exec-long-keep" 150;
  lost = ticks "exec-lost" 300;
}
