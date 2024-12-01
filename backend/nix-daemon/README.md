<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
<!-- SPDX-FileCopyrightText: 2024 embr <git@liclac.eu> -->

nix-daemon
==========

A library for talking directly to the [Nix](https://nixos.org) Daemon.

Client Usage
------------

To connect to a local `nix-daemon`, use a `nix::DaemonStore` (which implements the
`Store` trait):

```rust
use nix_daemon::{Store, Progress, nix::DaemonStore};

let mut store = DaemonStore::builder()
    .connect_unix("/nix/var/nix/daemon-socket/socket")
    .await?;
let is_valid_path = store.is_valid_path("/nix/store/...").result().await?;
```

Server Usage
------------

If you'd rather write your own `nix-daemon` compatible store, and expose it to existing
tools like `nix-build`, you can implement the `Store` trait yourself and use
`nix::DaemonProtocolAdapter`:

```rust
use tokio::net::UnixListener;
use nix_daemon::nix::{DaemonStore, DaemonProtocolAdapter};

// Accept a connection.
let listener = UnixListener::bind("/tmp/mystore.sock")?;
let (conn, _addr) = listener.accept().await?;

// This will just use `DaemonStore` to proxy to the normal daemon, but you can
// pass your own `Store` implementation here instead.
let mut store = DaemonStore::builder()
    .connect_unix("/nix/var/nix/daemon-socket/socket")
    .await?;

// Run the adapter!
let (cr, cw) = conn.into_split();
let mut adapter = DaemonProtocolAdapter::builder(&mut store)
    .adopt(cr, cw)
    .await?;
```

See [nix-supervisor](https://codeberg.org/gorgon/gorgon/src/branch/main/nix-supervisor) for a more complex example.

Limitations
-----------

- Not all opcodes are implemented (yet). Part of this is because in order to test them, we need to find somewhere they're actually used.
- Only Nix 2.15+ and Lix is supported at the moment, but support for 2.3 is high on the todo list. (And fairly easy to add.)

Contributing
------------

Please see the [main README](..#contributing).

---

[<img src="https://nlnet.nl/logo/banner.svg" width="200" alt="NLNet Foundation logo" />](https://nlnet.nl/)
[<img src="https://nlnet.nl/image/logos/NGI0Entrust_tag.svg" width="200" alt="NGI Zero Entrust logo" />](https://nlnet.nl/NGI0/)

This project was funded through the NGI0 Entrust Fund, a fund established by NLnet with financial support from the European Commission's Next Generation Internet programme, under the aegis of DG Communications Networks, Content and Technology under grant agreement Nº 101069594.
