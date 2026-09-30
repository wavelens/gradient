/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs }: let
  toplevel = (pkgs.nixos [ ./container.nix ]).config.system.build.toplevel;
in pkgs.dockerTools.buildLayeredImage {
  name = "gradient-standalone";
  tag = "latest";

  # The worker's nix-daemon must see the image's store paths as valid, or it
  # deletes them as garbage before substituting.
  extraCommands = ''
    mkdir -p nix/var/nix/gcroots tmp var/lib
    chmod 1777 tmp
    NIX_REMOTE="local?root=$PWD" USER=nobody \
      ${pkgs.nix}/bin/nix-store --load-db < ${pkgs.closureInfo { rootPaths = [ toplevel ]; }}/registration
    ln -s ${toplevel} nix/var/nix/gcroots/standalone
  '';

  config = {
    Cmd = [ "${toplevel}/init" ];
    Env = [ "container=docker" "PATH=/run/wrappers/bin:${toplevel}/sw/bin" ];
    ExposedPorts."80/tcp" = { };
    Volumes."/var/lib" = { };
    StopSignal = "SIGRTMIN+3";
  };
}
