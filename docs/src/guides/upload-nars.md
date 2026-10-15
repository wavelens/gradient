# Upload NARs

Build store paths outside of Gradient and push them into a cache with the `gradient` CLI.

**Requirements:**

- A cache with the Write role, see [Share a Cache](share-a-cache.md)
- The CLI: `nix shell github:wavelens/gradient/latest#gradient-cli`

## 1. Log In

```sh
gradient login https://gradient.example.com
```

The command opens a browser window for confirmation. After approval, the CLI stores the server address and the token for later use.

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

Check the upload with the CLI:

```sh
gradient cache nar list main --package hello
```

The cache page shows the same entries under **NARs**, filterable by hash and package.

## Manage NARs

| Command | Effect |
|---|---|
| `gradient cache nar list <cache>` | List NARs. Filter with `--hash` and `--package`, sort with `--sort`. |
| `gradient cache nar show <cache> <hash>` | Show one NAR: store path, sizes, signature and fetch count. |
| `gradient cache nar stats <cache>` | Show the NAR count and total size. |
| `gradient cache nar delete <cache> <hash>` | Delete the NAR from the cache. |

A NAR can live in several caches at once. Storage of a NAR continue until the last cache delete it. A delete in one cache leave all other caches intact.

## Next Steps

- [Share a Cache](share-a-cache.md): reuse the uploaded paths on other machines
- [Caches](../concepts/caches.md): upstream caches, pull-through and substitution order
