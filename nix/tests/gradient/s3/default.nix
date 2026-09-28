/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  statePwHash = pkgs.writeText "state-pw-hash" "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQxMjM0NQ$hIKBEy9SOWlnAlcwUv2PLPBdsMkKhVlCyjTxaWIK+v4";
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-s3";
    globalTimeout = 1200;

    nodes.machine = { pkgs, lib, ... }: {
      imports = [ ../../../modules/gradient.nix ];

      virtualisation = {
        cores = 2;
        memorySize = 2048;
      };

      environment.systemPackages = with pkgs; [ busybox curl git jq minio-client ];

      # The fixture's builder is busybox's sh, named by store path in the flake.
      nix.settings.sandbox = false;

      services.minio = {
        enable = true;
        rootCredentialsFile = pkgs.writeText "minio-credentials" ''
          MINIO_ROOT_USER=gradient
          MINIO_ROOT_PASSWORD=gradientsecret
        '';
      };

      services.gitDaemon = {
        enable = true;
        basePath = "/var/lib/git/";
        exportAll = true;
      };

      environment.etc."gitconfig".text = ''
        [safe]
          directory = *
      '';

      services.gradient = {
        enable = true;
        frontend.enable = false;
        useTls = false;
        postgres.enable = true;
        domain = "gradient.local";
        secrets.jwtFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4");
        secrets.cryptFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
        metrics.tokenFile = toString (pkgs.writeText "metrics-token" "metricstoken");

        s3 = {
          enable = true;
          bucket = "gradient";
          region = "us-east-1";
          endpoint = "http://127.0.0.1:9000";
          accessKeyId = "gradient";
          secretAccessKeyFile = pkgs.writeText "s3-secret" "gradientsecret";
          virtualHostedStyle = false;
        };

        state.users.admin = {
          email = "admin@gradient.local";
          password_file = toString statePwHash;
          email_verified = true;
          superuser = true;
        };

        worker.enable = true;
      };

      systemd.services.gradient-server = {
        after = [ "minio-bucket.service" ];
        requires = [ "minio-bucket.service" ];
      };

      systemd.services.minio-bucket = {
        after = [ "minio.service" ];
        requires = [ "minio.service" ];
        wantedBy = [ "multi-user.target" ];
        path = [ pkgs.minio-client ];
        environment.HOME = "/run/minio-bucket";
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          RuntimeDirectory = "minio-bucket";
        };
        script = ''
          until mc alias set local http://127.0.0.1:9000 gradient gradientsecret; do sleep 1; done
          mc mb --ignore-existing local/gradient
        '';
      };

      systemd.tmpfiles.rules = [
        "d /var/lib/git 0755 git git"
        "L+ /var/lib/git/flake.nix 0755 git git - ${./flake.nix}"
      ];
    };

    testScript = builtins.readFile ./test.py;
  });
}
