 - Log OIDC Errors in journal error
 - Base (Default) Servers (for all orgs), configured only in state
 - move Evaluation errors to Entrypoint
 - when new git commit is detected, add it as queued (dont wait for the current evaluation to finish), abort the previous build if it is still running
 - pack_derivation_output is reading stdout to memory, but would be better to stream
 - disko image builder for unit test store: nix run .#diskoImage-qemu in libretoy (https://github.com/nix-community/disko/blob/5ad85c82cc52264f4beddc934ba57f3789f28347/lib/make-disk-image.nix#L168)
 - error pages - add a page for 404, 500, 502, 503, 504
 - poll rarly even when there are webhooks

 - save which builder was used for each build in the database
 - add a eval logLevel (seperate from build)
 - old docs for state configuration options
 - argon2 for user password hashing
 - system types need to be strings
 - docs: fix cache secret "main-cache:" before private key not needed (needs reformatting is not described in the docs)
 - oidc: with other port does not work
 - frontend hide login form if oidc is required and hide register form if oidc is required or registration is disabled
 - useTls is false but Set-Cookie has Secure flag
 - update updater script

