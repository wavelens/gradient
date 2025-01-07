/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, lib, ... }:
{
  # EXTREMELY UNSECURE Postgres DB setup.
  services.postgresql = {
    enable = true;
    package = pkgs.postgresql_17;
    enableJIT = true;
    enableTCPIP = true;
    authentication = ''
      #...
      #type database DBuser origin-address auth-method
      # ipv4
      host  all      all     0.0.0.0/0      trust
      # ipv6
      host all       all     ::0/0        trust
    '';

    settings = {
      # ssl = true;
      log_connections = true;
      logging_collector = true;
      log_disconnections = true;
      log_destination = lib.mkForce "syslog";
    };
  };

  # open firewall, needs to forwared port through the VM to.
  # allow communication from microvm port 5432 (postgres).
  networking.firewall.allowedTCPPorts = [ 5432 ];
}
