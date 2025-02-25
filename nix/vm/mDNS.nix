/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ ... }:
{
  services.avahi = { 
    enable = true;
    ipv4 = true;
    nssmdns4 = true;
    ipv6 = true;
    nssmdns6 = true;
    publish = {
      enable = true;
      addresses = true;
    };
  };
}
