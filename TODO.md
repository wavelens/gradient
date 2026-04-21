 - Log OIDC Errors in journal error
 - Base (Default) Workers (for all orgs, default deactivated), configured only in state
 - when new git commit is detected, add it as queued (dont wait for the current evaluation to finish), abort the previous build if it is still running

 - Final CI reporting (L2, L3, Q1, Q2) — needs injectable CI reporter to assert CiStatus::Success/Failure is sent when evaluation reaches terminal state. Currently the CI path is hard-coded to reporter_for_project() which creates reporters from DB config, not injectable.
 - Transitive cascade (M1) — needs proper graph walk test, but the current cascade_dependency_failed logic uses DB queries that are hard to stage for transitive walks with MockDB.
 - only grey out capabilities do not stright through

 - missing Child::kill() plumbing through WorkerPoolResolver.
 - save "env"."preferLocalBuild" in database parsed from .drv
 - save which worker was used for each build in the database
 - docs old for state configuration options
 - argon2 for user password hashing
 - docs: fix cache secret "main-cache:" before private key not needed (needs reformatting is not described in the docs)
 - update updater script

