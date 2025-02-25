/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ ... }:
{
  services = {
    loki = {
      enable = true;
      configFile = ./loki.yaml;
    };
  };
}
