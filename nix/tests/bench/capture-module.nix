/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  environment.systemPackages = with pkgs; [ curl flamegraph jq perf strace tcpdump ];
  boot.kernel.sysctl = {
    "kernel.perf_event_paranoid" = -1;
    "kernel.kptr_restrict" = 0;
  };
}
