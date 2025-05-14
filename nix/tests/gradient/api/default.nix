/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ ... }:
{
  name = "gradient-api";
  globalTimeout = 120;
  nodes = {
    machine = { config, pkgs, lib, ... }: {
      imports = [ ../../../modules/gradient.nix ];
      services = {
        gradient = {
          enable = true;
          listenAddr = "0.0.0.0";
          domain = "gradient.local";
          jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
          cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZAo=");
        };

        postgresql = {
          enable = true;
          package = pkgs.postgresql_17;
          enableJIT = true;
          enableTCPIP = true;
          ensureDatabases = [ "gradient" ];
          authentication = ''
            #...
            #type database DBuser origin-address auth-method
            # ipv4
            host  all      all     0.0.0.0/0      trust
            # ipv6
            host all       all     ::0/0        trust
          '';

          settings = {
            log_connections = true;
            logging_collector = true;
            log_disconnections = true;
            log_destination = lib.mkForce "syslog";
          };
        };
      };
    };
  };

  interactive.nodes = {
    machine = import ../../modules/debug-host.nix;
  };

  testScript = builtins.readFile ./test.py;
}
