{ pkgs, lib, ... }:
{
  # EXTREMELY UNSECURE Postgres DB setup.
  services.postgresql = {
    enable = true;
    package = pkgs.postgresql_17;
    enableJIT = true;
    enableTCPIP = true;
    settings = {
      # ssl = true;
      log_connections = true;
      logging_collector = true;
      log_disconnections = true;
      log_destination = lib.mkForce "syslog";
    };
    authentication = ''
      #...
      #type database DBuser origin-address auth-method
      # ipv4
      host  all      all     0.0.0.0/0      trust
      # ipv6
      host all       all     ::1/128        trust
    '';
  };

  #open firewall, needs to forwared port through the VM to.
  # allow communication from microvm port 5432 (postgres).
  networking.firewall.allowedTCPPorts = [ 5432 ];
}