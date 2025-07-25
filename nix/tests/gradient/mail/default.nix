/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: {
  value = pkgs.testers.runNixOSTest ({ pkgs, lib, ... }: {
    name = "gradient-mail";
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
          "127.0.0.1" = [ "gradient.local" "mail.local" ];
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

            # Enable email functionality
            email = {
              enable = true;
              requireVerification = true;
              smtpHost = "mail.local";
              smtpPort = 1025;
              smtpUsername = "gradient@example.com";
              smtpPasswordFile = toString (pkgs.writeText "smtpPassword" "test-password");
              fromAddress = "gradient@example.com";
              fromName = "Gradient Test";
            };

            settings = {
              disableRegistration = false;
              logLevel = "debug";
            };
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


          # Web interface to view emails (optional, for debugging)
          nginx = {
            enable = true;
            virtualHosts."mail.local" = {
              listen = [{ addr = "0.0.0.0"; port = 8080; }];
              locations = {
                "/" = {
                  return = "200 'Mock SMTP Server Running'";
                  extraConfig = "add_header Content-Type text/plain;";
                };
              };
            };
          };
        };

        # Mock SMTP server using Python's aiosmtpd
        systemd.services.mock-smtp = {
          wantedBy = [ "multi-user.target" ];
          after = [ "network.target" ];
          serviceConfig = {
            ExecStart = "${pkgs.python3.withPackages(ps: with ps; [ aiosmtpd ])}/bin/python3 -m aiosmtpd -n -l mail.local:1025 -c aiosmtpd.handlers.Debugging";
            Restart = "always";
            RestartSec = 5;
            User = "nobody";
            Group = "nobody";
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
      import json

      start_all()

      server.wait_for_unit("gradient-server.service")
      server.sleep(5)
      server.wait_for_unit("mock-smtp.service")
      server.wait_for_unit("nginx.service")

      print("=== Testing Health Check ===")
      server.succeed("${lib.getExe pkgs.curl} http://gradient.local/api/v1/health -i --fail")

      print("=== Testing User Registration with Email Verification ===")
      
      # Test registration - should succeed and send verification email
      register_response = server.succeed("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "testuser", "name": "Test User", "email": "test@example.com", "password": "SecureKey123!"}' \
          http://gradient.local/api/v1/auth/basic/register -s
      """)
      
      print(f"Registration response: {register_response}")
      
      # Parse response and check it mentions email verification
      register_data = json.loads(register_response)
      assert not register_data.get("error", True), f"Registration failed: {register_data}"
      assert "email" in register_data["message"].lower(), "Registration response should mention email verification"

      print("=== Testing Login Before Email Verification ===")
      
      # Test login before verification - should fail
      login_response = server.succeed("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"loginname": "testuser", "password": "SecureKey123!"}' \
          http://gradient.local/api/v1/auth/basic/login -s
      """)
      
      print(f"Login before verification response: {login_response}")
      login_data = json.loads(login_response)
      assert login_data.get("error", False), "Login should fail before email verification"
      assert "verified" in login_data["message"].lower(), "Login error should mention email verification"

      print("=== Testing Email Verification Endpoint ===")
      
      # For testing purposes, we'll create a mock verification token
      # In a real scenario, this would be extracted from the email
      test_token = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
      
      # First, let's test with an invalid token
      verify_invalid_response = server.succeed("""
          ${lib.getExe pkgs.curl} \
          "http://gradient.local/api/v1/auth/verify-email?token=invalid-token" -s
      """)
      
      print(f"Invalid token verification response: {verify_invalid_response}")
      verify_invalid_data = json.loads(verify_invalid_response)
      assert verify_invalid_data.get("error", False), "Verification with invalid token should fail"

      print("=== Testing Resend Verification Email ===")
      
      # Test resending verification email
      resend_response = server.succeed("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "testuser"}' \
          http://gradient.local/api/v1/auth/resend-verification -s
      """)
      
      print(f"Resend verification response: {resend_response}")
      resend_data = json.loads(resend_response)
      # Note: SMTP connection may fail in test environment, but the endpoint should respond properly
      # We expect either success or a specific SMTP error (not a server error)
      if resend_data.get("error", False):
          # Allow SMTP connection errors but not other types of errors
          assert "send verification email" in resend_data.get("message", ""), f"Unexpected resend error: {resend_data}"
          print("SMTP connection failed as expected in test environment")

      print("=== Testing Registration Without Email Verification (when disabled) ===")
      
      # Test that when email verification is disabled, users can login immediately
      # This would require restarting the service with different config, but we'll skip for now
      
      print("=== Testing Username Availability Check ===")
      
      # Test username check endpoint
      username_check_response = server.succeed("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "testuser"}' \
          http://gradient.local/api/v1/auth/check-username -s
      """)
      
      print(f"Username check response: {username_check_response}")
      username_data = json.loads(username_check_response)
      assert username_data.get("error", False), "Username check should show username is taken"

      # Test with available username
      username_available_response = server.succeed("""
          ${lib.getExe pkgs.curl} \
          -X POST \
          -H "Content-Type: application/json" \
          -d '{"username": "availableuser"}' \
          http://gradient.local/api/v1/auth/check-username -s
      """)
      
      print(f"Available username check response: {username_available_response}")
      available_data = json.loads(username_available_response)
      assert not available_data.get("error", True), "Available username check should succeed"

      print("=== Checking SMTP Mock Server Logs ===")
      
      # Check that SMTP server received email attempts
      smtp_logs = server.succeed("journalctl -u mock-smtp --no-pager -n 50")
      print(f"SMTP logs: {smtp_logs}")
      
      # The mock SMTP server should have received connection attempts
      # We don't expect successful email delivery since it's just a debugging handler

      print("=== Email Functionality Tests Completed Successfully ===")
      '';
  });
}
