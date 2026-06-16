<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>

SPDX-License-Identifier: AGPL-3.0-only
-->

# Caches

Gradient serves build outputs from per-organization binary caches. Each cache
exposes a substituter URL and a set of trusted public keys that clients add to
their Nix configuration. The cache detail page renders the exact snippet for a
given cache.

## Authentication

Private caches require credentials. Install them on the client as root with the
Gradient CLI:

```bash
nix run wavelens/gradient#gradient-cli -- cache install-netrc \
  --server <SERVER_URL> --token <YOUR_TOKEN> --cache <CACHE_NAME>
```

Replace `<YOUR_TOKEN>` with your API key or login token. The command writes the
credentials to `/etc/nix/netrc` so the Nix daemon can authenticate against the
cache.

### Declarative netrc

To manage the netrc file declaratively on NixOS, render it from a secret. The
example below uses [sops-nix](https://github.com/Mic92/sops-nix):

```nix
{ config, ... }: {
  sops.secrets."gradient-api-token" = { };
  sops.templates."nix-netrc" = {
    content = ''
      machine <SERVER_HOSTNAME>
      login gradient
      password ${config.sops.placeholder."gradient-api-token"}
    '';
    owner = "<YOUR_USERNAME>";
    path = "/etc/nix/netrc";
  };
}
```

Replace `<SERVER_HOSTNAME>` with the Gradient host and `<YOUR_USERNAME>` with the
user that should be able to use this cache.
