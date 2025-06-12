{ ...}: {
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
