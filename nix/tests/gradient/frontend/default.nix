/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-frontend";
    globalTimeout = 600;

    defaults = {
      networking.firewall.enable = false;
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
      virtualisation = {
        writableStore = true;
        memorySize = 2048;
        diskSize = 4096;
      };
    };

    nodes = {
      machine = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" ];
        };

        services = {
          gradient = {
            enable = true;
            frontend.enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");
            settings = {
              logLevel = "debug";
              disableRegistration = false;
            };
          };

          postgresql = {
            package = pkgs.postgresql_18;
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
              log_connections = true;
              logging_collector = true;
              log_disconnections = true;
              log_destination = lib.mkForce "syslog";
            };
          };

          # Minimal X11 setup for headless browser testing
          xserver = {
            enable = true;
            displayManager.startx.enable = true;
          };
        };

        # Create test user
        users.users.testuser = {
          isNormalUser = true;
          password = "test";
          extraGroups = [ "wheel" ];
        };

        # Install testing packages with proper browser setup
        environment.systemPackages = with pkgs; [
          chromium
          chromedriver
          firefox
          geckodriver
          curl
          jq
          xvfb-run
          # Custom Python environment with all needed packages
          (python3.withPackages (ps: with ps; [
            selenium
            webdriver-manager
            requests
            beautifulsoup4
            lxml
            pytest
          ]))
        ];
        
        # Set up browser environment variables
        environment.variables = {
          CHROME_BIN = "${pkgs.chromium}/bin/chromium";
          CHROME_DRIVER = "${pkgs.chromedriver}/bin/chromedriver";
          FIREFOX_BIN = "${pkgs.firefox}/bin/firefox";
          GECKO_DRIVER = "${pkgs.geckodriver}/bin/geckodriver";
        };

        # Configure display for headless testing
        systemd.services.xvfb = {
          wantedBy = [ "multi-user.target" ];
          after = [ "network.target" ];
          serviceConfig = {
            ExecStart = "${pkgs.xvfb-run}/bin/Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp";
            Restart = "always";
            RestartSec = 5;
            User = "testuser";
            Group = "users";
          };
        };

        nix.settings = {
          max-jobs = 0;
        };
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = builtins.readFile ./test-selenium.py;
  });
}
