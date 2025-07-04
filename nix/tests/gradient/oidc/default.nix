/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-oidc";
    globalTimeout = 480;

    defaults = {
      networking.firewall.enable = false;
      virtualisation.writableStore = true;
      documentation.enable = false;
      nix.settings.substituters = lib.mkForce [ ];
    };

    nodes = {
      server = { config, pkgs, lib, ... }: {
        imports = [
          ../../../modules/gradient.nix
        ];

        networking.hosts = {
          "127.0.0.1" = [ "gradient.local" "oidc.local" ];
        };

        services = {
          gradient = {
            enable = true;
            serveCache = true;
            configureNginx = true;
            configurePostgres = true;
            domain = "gradient.local";
            jwtSecretFile = toString (pkgs.writeText "jwtSecret" "b68a8eaa8ebcff23ebaba1bd74ecb8a2eb7ba959570ff8842f148207524c7b8d731d7a1998584105e951599221f9dcd20e41223be17275ca70ab6f7e6ecafa8d4f8905623866edb2b344bd15de52ccece395b3546e2f00644eb2679cf7bdaa156fd75cc5f47c34448cba19d903e68015b1ad3c8e9d04862de0a2c525b6676779012919fa9551c4746f9323ab207aedae86c28ada67c901cae821eef97b69ca4ebe1260de31add34d8265f17d9c547e3bbabe284d9cadcc22063ee625b104592403368090642a41967f8ada5791cb09703d0762a3175d0fe06ec37822e9e41d0a623a6349901749673735fdb94f2c268ac08a24216efb058feced6e785f34185a");
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZAo=");
            
            # Enable OIDC
            oauth = {
              enable = true;
              required = true;
              clientId = "gradient-test";
              clientSecretFile = toString (pkgs.writeText "oauthSecret" "test-secret");
              scopes = [ "openid" "profile" "email" ];
              tokenUrl = "http://oidc.local:8080/oauth/token";
              authUrl = "http://oidc.local:8080/oauth/authorize";
              apiUrl = "http://oidc.local:8080/userinfo";
            };
            
            settings.disableRegistration = true;
          };

          postgresql = {
            package = pkgs.postgresql_17;
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

          # Mock OIDC provider using nginx
          nginx = {
            enable = true;
            virtualHosts."oidc.local" = {
              listen = [{ addr = "0.0.0.0"; port = 8080; }];
              locations = {
                "/.well-known/openid-configuration" = {
                  return = "200 '${builtins.toJSON {
                    issuer = "http://oidc.local:8080";
                    authorization_endpoint = "http://oidc.local:8080/oauth/authorize";
                    token_endpoint = "http://oidc.local:8080/oauth/token";
                    userinfo_endpoint = "http://oidc.local:8080/userinfo";
                    jwks_uri = "http://oidc.local:8080/.well-known/jwks.json";
                    response_types_supported = ["code"];
                    subject_types_supported = ["public"];
                    id_token_signing_alg_values_supported = ["RS256"];
                  }}'";
                  extraConfig = "add_header Content-Type application/json;";
                };
                
                "/oauth/authorize" = {
                  return = "302 http://gradient.local/api/v1/auth/oidc/callback?code=test-auth-code&state=$arg_state";
                };
                
                "/oauth/token" = {
                  extraConfig = ''
                    limit_except POST {
                      deny all;
                    }
                    add_header Content-Type application/json always;
                    return 200 '${builtins.toJSON {
                      access_token = "test-access-token";
                      token_type = "Bearer";
                      expires_in = 3600;
                      id_token = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ0ZXN0LXVzZXIiLCJuYW1lIjoiVGVzdCBVc2VyIiwiZW1haWwiOiJ0ZXN0QGV4YW1wbGUuY29tIiwiaWF0IjoxNTE2MjM5MDIyfQ.test-signature";
                    }}';
                  '';
                };
                
                "/userinfo" = {
                  return = "200 '${builtins.toJSON {
                    sub = "test-user";
                    name = "Test User";
                    email = "test@example.com";
                    preferred_username = "testuser";
                  }}'";
                  extraConfig = "add_header Content-Type application/json;";
                };
              };
            };
          };
        };

        nix.settings = {
          max-jobs = 0;
        };

        # Add OIDC discovery URL as environment variable
        systemd.services.gradient-server.environment.GRADIENT_OIDC_DISCOVERY_URL = "http://oidc.local:8080";
      };
    };

    interactive.nodes = {
      server = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      start_all()

      server.wait_for_unit("gradient-server.service")
      server.wait_for_unit("nginx.service")

      server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

      server.fail("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "test", "name": "Test User", "email": "test@localhost.localdomain", "password": "password"}' \
          http://gradient.local/api/v1/auth/basic/register -i --fail
      """)

      print("=== Testing OIDC Authentication Flow ===")
      auth_redirect = server.succeed("""
        ${lib.getExe pkgs.curl} -s -o /dev/null -w "%{redirect_url}" -i \
          "http://gradient.local/api/v1/auth/oidc/login?redirect_uri=http://gradient.local/dashboard" --fail
      """)
      print(f"Auth redirect URL: {auth_redirect}")

      print("=== Testing OIDC Callback ===")
      callback_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s -i \
          "http://gradient.local/api/v1/auth/oidc/callback?code=test-auth-code&state=test-state"
      """)
      print(f"Callback response: {callback_response}")
      
      # Check that the callback didn't return an error
      if "500 Internal Server Error" in callback_response or '"error":true' in callback_response:
          raise Exception(f"OIDC callback failed with error: {callback_response}")

      # Test that gradient-cli can handle OIDC authentication
      print("=== Testing CLI with OIDC ===")

      # Note: In a real scenario, the CLI would need to handle the OIDC flow
      # For testing purposes, we'll verify the endpoints are available
      server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/auth/oidc/login -s --fail")

      print("=== OIDC Tests Completed Successfully ===")
      '';
  });
}
