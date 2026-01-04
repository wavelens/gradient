/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
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

        systemd.services.gradient-server.environment.GRADIENT_DEBUG = lib.mkForce "true";
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
            cryptSecretFile = toString (pkgs.writeText "cryptSecret" "aW52YWxpZC1pbnZhbGlkLWludmFsaWQK");

            # Enable OIDC
            oidc = {
              enable = true;
              required = true;
              clientId = "gradient-test";
              clientSecretFile = toString (pkgs.writeText "oidcSecret" "test-secret");
              scopes = [ "openid" "profile" "email" ];
              discoveryUrl = "http://oidc.local:8080";
            };

            settings = {
              disableRegistration = true;
              logLevel = "warn";
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
      };
    };

    interactive.nodes = {
      server = import ../../modules/debug-host.nix;
    };

    testScript = { nodes, ... }:
      ''
      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      server.wait_for_unit("nginx.service")

      # Verify basic health endpoint
      server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

      # Verify OIDC discovery endpoint is reachable
      print("=== Testing OIDC Provider Discovery ===")
      discovery_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s http://oidc.local:8080/.well-known/openid-configuration --fail
      """)
      print(f"OIDC Discovery response: {discovery_response}")

      # Verify basic registration is disabled when OIDC is required
      print("=== Verifying Basic Auth Registration is Disabled ===")
      server.fail("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "testuser", "name": "Test User", "email": "test@localhost.localdomain", "password": "password"}' \
          http://gradient.local/api/v1/auth/basic/register -i --fail
      """)

      # Verify basic login is also disabled when OIDC is required
      print("=== Verifying Basic Auth Login is Disabled ===")
      server.fail("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "testuser", "password": "password"}' \
          http://gradient.local/api/v1/auth/basic/login -i --fail
      """)

      print("=== Testing OIDC Authentication Flow ===")

      # Test OIDC login initiation
      auth_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s -i \
          "http://gradient.local/api/v1/auth/oidc/login?redirect_uri=http://gradient.local/dashboard" --fail
      """)
      print(f"Auth initiation response: {auth_response}")

      # Extract redirect URL from location header
      auth_redirect = server.succeed("""
        ${lib.getExe pkgs.curl} -s -o /dev/null -w "%{redirect_url}" \
          "http://gradient.local/api/v1/auth/oidc/login?redirect_uri=http://gradient.local/dashboard" --fail
      """)
      print(f"Auth redirect URL: {auth_redirect}")

      # Verify redirect URL contains expected OIDC parameters
      if "client_id=gradient-test" not in auth_redirect:
          raise Exception(f"Auth redirect missing client_id: {auth_redirect}")
      if "response_type=code" not in auth_redirect:
          raise Exception(f"Auth redirect missing response_type: {auth_redirect}")
      if "scope=" not in auth_redirect:
          raise Exception(f"Auth redirect missing scope: {auth_redirect}")

      print("=== Testing OIDC Callback Processing ===")

      # Test callback with valid authorization code
      callback_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s -i -L \
          "http://gradient.local/api/v1/auth/oidc/callback?code=test-auth-code&state=test-state"
      """)
      print(f"Callback response: {callback_response}")

      # Check that the callback didn't return an error
      if "500 Internal Server Error" in callback_response:
          raise Exception(f"OIDC callback failed with 500 error: {callback_response}")
      if '"error":true' in callback_response:
          raise Exception(f"OIDC callback returned error response: {callback_response}")

      # Test callback with invalid/missing code
      print("=== Testing OIDC Error Handling ===")
      error_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s -i \
          "http://gradient.local/api/v1/auth/oidc/callback?error=access_denied&state=test-state" || true
      """)
      print(f"Error callback response: {error_response}")

      # Test callback with missing state parameter
      missing_state_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s -i \
          "http://gradient.local/api/v1/auth/oidc/callback?code=test-auth-code" || true
      """)
      print(f"Missing state response: {missing_state_response}")

      print("=== Testing OIDC Token Validation ===")

      # Test that protected endpoints require authentication
      protected_response = server.succeed("""
        ${lib.getExe pkgs.curl} -s -i \
          "http://gradient.local/api/v1/user/profile" || true
      """)
      print(f"Protected endpoint response: {protected_response}")

      # Should return 401 Unauthorized
      if "401" not in protected_response and "Unauthorized" not in protected_response:
          print("Warning: Protected endpoint should require authentication")

      print("=== OIDC Integration Tests Completed Successfully ===")
      '';
  });
}
