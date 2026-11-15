/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ pkgs, ... }: let
  frontendPlaywrightTestsSource = pkgs.lib.cleanSourceWith {
    filter = name: type: !(type == "directory" && builtins.elem (baseNameOf name) [ "test-results" "signed-in-browser-storage-states" "playwright-report" ]);
    src = pkgs.lib.cleanSource ../../../../frontend/playwright-tests;
  };
in {
  value = pkgs.testers.runNixOSTest ({ pkgs, ... }: {
    name = "gradient-web_ui_browser";
    globalTimeout = 1800;

    nodes = {
      machine = {
        imports = [ ../api/shared-test-machine.nix ];

        virtualisation = {
          memorySize = 4096;
          cores = 4;
        };

        environment.systemPackages = [ pkgs.playwright-test ];
      };
    };

    interactive.nodes = {
      machine = import ../../modules/debug-host.nix;
    };

    testScript = ''
      PLAYWRIGHT_SPEC_DIRECTORY_STORE_PATH = "${frontendPlaywrightTestsSource}"
      PLAYWRIGHT_TEST_NODE_MODULES_STORE_PATH = "${pkgs.playwright-test}/lib/node_modules"
      PLAYWRIGHT_CHROMIUM_BROWSERS_STORE_PATH = "${pkgs.playwright-driver.browsers}"
    '' + builtins.readFile ./test.py;
  });
}
