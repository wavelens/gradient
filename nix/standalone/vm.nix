/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ modulesPath, ... }: {
  imports = [
    ./default.nix
    "${modulesPath}/virtualisation/qemu-vm.nix"
  ];

  networking.firewall.allowedTCPPorts = [ 80 ];
  services.getty.autologinUser = "root";

  virtualisation = {
    cores = 4;
    memorySize = 8192;
    diskSize = 32768;
    graphics = false;
    writableStoreUseTmpfs = false;
    forwardPorts = [
      { from = "host"; host.address = "127.0.0.1"; host.port = 8080; guest.port = 80; }
    ];
  };
}
