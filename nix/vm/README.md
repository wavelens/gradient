# Setup an virtual bridge interface, according to this document.

Import the following config to allow the VM to connect to the Internet and you be able to open the postgres DB.
```
{ ... }:
{
  systemd.network = {
    netdevs = {
      # Create the bridge interface
      "20-virbr0" = {
        bridgeConfig.STP = true;
        netdevConfig = {
          Kind = "bridge";
          Name = "virbr0";
        };
      };
    };
    networks = {
      # Connect the bridge ports to the bridge
      "30-skyflake" = {
        matchConfig.Name = [
          "enp*"
          "vm-*"
       ];
       bridge = [ "virbr0" ];
       linkConfig.RequiredForOnline = "enslaved";
      };
      # Configure the bridge for its desired function
      "40-virbr0" = {
        name = "virbr0";
        DHCP = "yes";
        bridgeConfig = {};
        # Disable address autoconfig when no IP configuration is required
        networkConfig.LinkLocalAddressing = "no";
        linkConfig = {
          # or "routable" with IP addresses configured
          RequiredForOnline = "routable";
        };
      };
    };
  };
}
```
# Connecting to the VM
connect with `psql -U postgres -h ${IP_OF_VM}`
or use the domain `gradient-dev.local`
