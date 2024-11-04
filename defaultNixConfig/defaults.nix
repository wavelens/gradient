{
  nix = {
    settings = {
      # fetch github-prebuilt microvm-kernels
      substituters = [
        "https://microvm.cachix.org"
      ];
      trusted-public-keys = [
        "microvm.cachix.org-1:oXnBc6hRE3eX5rSYdRyMYXnfzcCxC7yKPTbZXALsqys="
      ];
    };

    extraOptions = ''
      experimental-features = nix-command flakes
      builders-use-substitutes = true
    '';
  };
  
}

