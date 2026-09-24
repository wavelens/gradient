/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ config, lib, ... }:
let
  cfg = config.services.gradient-daemon-mock;
in
{
  options.services.gradient-daemon-mock = {
    enable = lib.mkEnableOption "gradient-daemon's mock backend in place of nix-daemon";
    package = lib.mkOption { type = lib.types.package; };
    config = lib.mkOption {
      type = lib.types.path;
      description = "Resolved daemon config, from store-spec's toDaemonConfig.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.nix-daemon.enable = lib.mkForce false;
    systemd.sockets.nix-daemon.enable = lib.mkForce false;
    environment.variables.NIX_REMOTE = "daemon";
    environment.systemPackages = [ cfg.package ];

    systemd.sockets.gradient-daemon = {
      wantedBy = [ "sockets.target" ];
      before = [ "gradient-worker.service" ];
      requiredBy = [ "gradient-worker.service" ];
      socketConfig = {
        ListenStream = "/nix/var/nix/daemon-socket/socket";
        SocketMode = "0666";
      };
    };

    systemd.services.gradient-daemon = {
      wantedBy = [ "multi-user.target" ];
      requires = [ "gradient-daemon.socket" ];
      environment.RUST_LOG = "info";
      serviceConfig = {
        ExecStart = "${lib.getExe' cfg.package "gradient-daemon"} serve --backend mock --spec ${cfg.config}";
        # NixOS binds /nix/store read-only; nix-daemon remounts it in its own namespace, so must we.
        ReadWritePaths = [ "/nix/store" ];
        Restart = "on-failure";
      };
    };
  };
}
