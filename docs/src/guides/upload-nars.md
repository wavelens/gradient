# Upload NARs

Store paths built outside Gradient, pushed into a cache with the `gradient` CLI.

**Requirements:**

- A cache and the Write role on the cache, see [Share a Cache](share-a-cache.md)
- The CLI: `nix shell github:wavelens/gradient#gradient-cli`

## 1. Log In

```sh
gradient login https://gradient.example.com
```

The CLI opens the browser to confirm the login and stores the server and the token.

## 2. Upload

```sh
gradient cache upload main $(readlink -f result)
```

- The CLI uploads the path together with the full runtime closure, dependencies first.
- `--no-closure` uploads only the named paths.
- Paths are full store paths; `readlink -f` resolves a `result` link.

Large NARs go up in 32 MiB chunks, below the reverse proxy's body limit. The server caps a single NAR at [`services.gradient.nar.maxUploadSize`](../reference/configuration.md#nar), 512 MiB by default.

??? note "Machines without Nix"
    A NAR dumped elsewhere uploads together with the matching narinfo:

    ```sh
    gradient cache upload main --nar-file hello.nar --narinfo hello.narinfo
    ```

## Verify Deployment

```sh
gradient cache nar list main --package hello
```

The same list shows in the UI under **NARs** on the cache page, with filters by hash and package.

## Manage NARs

| Command | Effect |
|---|---|
| `gradient cache nar list <cache>` | Lists NARs, filter with `--hash`, `--package`, sort with `--sort` |
| `gradient cache nar show <cache> <hash>` | Shows one NAR: store path, sizes, signature and fetch count |
| `gradient cache nar stats <cache>` | Shows the NAR count and total size |
| `gradient cache nar delete <cache> <hash>` | Removes the NAR from the cache |

A NAR held by several caches stays stored until the last cache deletes the NAR; deleting from one cache never breaks another.

## Next Steps

- [Share a Cache](share-a-cache.md): use the uploaded paths on other machines
- [Caches](../concepts/caches.md): upstreams, pull-through and substitution order
