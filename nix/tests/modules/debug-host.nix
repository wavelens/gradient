# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only

{ ... }: {
  interactive.virtualisation.graphics = false;
  security.pam.services.sshd.allowNullPassword = true;
  services.openssh = {
    enable = true;
    settings = {
      PermitRootLogin = "yes";
      PermitEmptyPasswords = "yes";
    };
  };

  virtualisation.forwardPorts = [{
    from = "host";
    host.port = 2222;
    guest.port = 22;
  }];
}
