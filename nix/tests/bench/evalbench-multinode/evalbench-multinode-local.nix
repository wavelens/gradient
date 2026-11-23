/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ self, pkgs }: let
  test = import ./mk.nix {
    inherit self pkgs;
    topology = import ../../harness/topologies/direct.nix;
    useLocalVmNixStoreImageAndDiskWritableStore = true;
  };
in
pkgs.writeShellApplication {
  name = "evalbench-multinode-local";
  runtimeInputs = [ pkgs.coreutils ];
  text = ''
    : "''${LOCAL_BENCHMARK_VM_STATE_DIR:=/tmp/evalbench-multinode}"
    : "''${BENCHMARK_RESULT_DIRECTORY:=$LOCAL_BENCHMARK_VM_STATE_DIR/results-$(date +%Y%m%d-%H%M%S)}"
    mkdir -p "$LOCAL_BENCHMARK_VM_STATE_DIR" "$BENCHMARK_RESULT_DIRECTORY"
    export XDG_RUNTIME_DIR="$LOCAL_BENCHMARK_VM_STATE_DIR" TMPDIR="$LOCAL_BENCHMARK_VM_STATE_DIR"
    ${test.driver}/bin/nixos-test-driver --keep-machine-state -o "$BENCHMARK_RESULT_DIRECTORY" "$@"
    echo "$BENCHMARK_RESULT_DIRECTORY"
  '';
}
