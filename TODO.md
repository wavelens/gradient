 - Log OIDC Errors in journal error
 - Base (Default) Workers (for all orgs), configured only in state
 - when new git commit is detected, add it as queued (dont wait for the current evaluation to finish), abort the previous build if it is still running
 - pack_derivation_output is reading stdout to memory, but would be better to stream
 - disko image builder for unit test store: nix run .#diskoImage-qemu in libretoy (https://github.com/nix-community/disko/blob/5ad85c82cc52264f4beddc934ba57f3789f28347/lib/make-disk-image.nix#L168)
 - be aware of the don't substitute bit

 - Final CI reporting (L2, L3, Q1, Q2) — needs injectable CI reporter to assert CiStatus::Success/Failure is sent when evaluation reaches terminal state. Currently the CI path is hard-coded to reporter_for_project() which creates reporters from DB config, not injectable.
 - Transitive cascade (M1) — needs proper graph walk test, but the current cascade_dependency_failed logic uses DB queries that are hard to stage for transitive walks with MockDB.
 -  make score_candidates non-blocking for the dispatch loop. The proper fix is to spawn each JobListChunk / JobOffer /
  RequestAllScores scoring pass into its own task that holds Arc<Mutex<HashMap>> clones of candidates and last_scores, computes scores, and sends RequestJobChunk via the writer. The dispatch loop would then return to select! immediately and stay responsive to CacheStatus (and everything else) regardless of how long scoring takes. I left this as task #8 since it touches the worker-loop state model and warrants its own focused change.
 - aborting jobs does not work when in EvaluatingDerivation state

 - build log completed builds value is increasing, but shoudn't, because the updates adds all builds to the completed value (not only the new ones)
 - save which worker was used for each build in the database
 - old docs for state configuration options
 - argon2 for user password hashing
 - system types need to be strings
 - docs: fix cache secret "main-cache:" before private key not needed (needs reformatting is not described in the docs)
 - update updater script

