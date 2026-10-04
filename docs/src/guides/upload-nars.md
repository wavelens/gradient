# Upload NARs

Store paths built outside Gradient, pushed into a cache with the `gradient` CLI.

**Requirements:**

- A cache and the Write role on the cache, see [Share a Cache](share-a-cache.md)
- The CLI: `nix shell github:wavelens/gradient/latest#gradient-cli`

## 1. Log In

```sh
gradient login https://gradient.example.com
```

The CLI will open the browser to confirm the login. The CLI is then storing the server and the token.

## 2. Upload

```sh
gradient cache upload main $(readlink -f result)
```

- The CLI will upload the path together with the full runtime closure, dependencies first.
- `--no-closure` will upload only the named paths.
- Paths are full store paths.
- `readlink -f` can resolve a `result` link.

Large NARs go up in 32 MiB chunks, below the reverse proxy's body limit. The server will cap a single NAR at [`services.gradient.nar.maxUploadSize`](../reference/configuration.md#nar), 512 MiB by default.

??? note "Machines Without Nix"
    A NAR dumped elsewhere is uploadable together with the matching narinfo. This mode is also the only upload mode of the [static binary](../reference/cli.md#install).

    ```sh
    gradient cache upload main --nar-file hello.nar --narinfo hello.narinfo
    ```

## Verify Deployment

```sh
gradient cache nar list main --package hello
```

The UI will show the same list under **NARs** on the cache page, with filters by hash and package.

## Manage NARs

| Command | Effect |
|---|---|
| `gradient cache nar list <cache>` | Listing NARs, filtered with `--hash` and `--package`, sorted with `--sort` |
| `gradient cache nar show <cache> <hash>` | One NAR: store path, sizes, signature and fetch count |
| `gradient cache nar stats <cache>` | The NAR count and total size |
| `gradient cache nar delete <cache> <hash>` | Removing the NAR from the cache |

NARs held by more than one cache stay stored until deleted from the last cache. A deletion from one cache can never break another.

## Next Steps

- [Share a Cache](share-a-cache.md): use the uploaded paths on other machines
- [Caches](../concepts/caches.md): upstream caches, pull-through and substitution order
