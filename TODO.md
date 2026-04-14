 - Log OIDC Errors in journal error
 - Base (Default) Servers (for all orgs), configured only in state
 - move Evaluation errors to Entrypoint
 - when new git commit is detected, add it as queued (dont wait for the current evaluation to finish), abort the previous build if it is still running
 - pack_derivation_output is reading stdout to memory, but would be better to stream
 - disko image builder for unit test store: nix run .#diskoImage-qemu in libretoy (https://github.com/nix-community/disko/blob/5ad85c82cc52264f4beddc934ba57f3789f28347/lib/make-disk-image.nix#L168)
 - error pages - add a page for 404, 500, 502, 503, 504
 - poll rarly even when there are webhooks
 - accept only connections with same proto version
 - workers need to be restared (maybe they crash)
 - prioritzie workers when oom killing
 - when commit id is same no need to re-evaluate
 - dropdown for "ci-provider" integration in frontend is different css from the other dropdowns
 - is the worker token aes encrypted in the database and handled as a secret? if not, encrypt it
 - "Genrate Webhook Secret" button in frontend is not reapearing after saving and then refreshing the page

 - Final CI reporting (L2, L3, Q1, Q2) — needs injectable CI reporter to assert CiStatus::Success/Failure is sent when evaluation reaches terminal state. Currently the CI path is hard-coded to reporter_for_project() which creates reporters from DB config, not injectable.
 - Transitive cascade (M1) — needs proper graph walk test, but the current cascade_dependency_failed logic uses DB queries that are hard to stage for transitive walks with MockDB.

 - save which builder was used for each build in the database
 - add a eval logLevel (seperate from build)
 - old docs for state configuration options
 - argon2 for user password hashing
 - system types need to be strings
 - docs: fix cache secret "main-cache:" before private key not needed (needs reformatting is not described in the docs)
 - oidc: with other port does not work
 - frontend hide login form if oidc is required and hide register form if oidc is required or registration is disabled
 - update updater script

