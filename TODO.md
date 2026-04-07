 - before a build is building check again if the build is already built
 - Log OIDC Errors in journal error
 - Add Super User
 - Add Non-SSH server as default
 - enable_registration should be used (also in frontend to modify the UI)
 - Base (Default) Servers (for all orgs), configured only in state
 - if git link is http(s) make link clickable

 Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.347512Z DEBUG ThreadId(21) schedule_evaluation{evaluation=Model { id: 43951954-036c-46cf-aeed-1e8209d7a347, project: Some(0f5ae8b9-567e-4dbe-80c1-5237c0b6f62a), repository: "https://github.com/wavelens/gradient", commit: f4bf55f5-df19-48b5-a470-edf3a9e1a7ef, wildcard: "checks.x86_64-linux.*", status: Queued, previous: Some(3a6af860-b703-422d-aeb7-0d5039134896), next: None, created_at: 2026-04-07T00:19:10.379501, updated_at: 2026-04-07T00:19:10.379501, error: None } evaluation_id=43951954-036c-46cf-aeed-1e8209d7a347}:evaluate{evaluation=Model { id: 43951954-036c-46cf-aeed-1e8209d7a347, project: Some(0f5ae8b9-567e-4dbe-80c1-5237c0b6f62a), repository: "https://github.com/wavelens/gradient", commit: f4bf55f5-df19-48b5-a470-edf3a9e1a7ef, wildcard: "checks.x86_64-linux.*", status: Queued, previous: Some(3a6af860-b703-422d-aeb7-0d5039134896), next: None, created_at: 2026-04-07T00:19:10.379501, updated_at: 2026-04-07T00:19:10.379501, error: None } evaluation_id=43951954-036c-46cf-aeed-1e8209d7a347}:prefetch_flake{repository=git+https://github.com/wavelens/gradient?rev=900a095e2b1ab94fd5cf9338cf9128e72236151e}: core::sources: HTTPS repository – skipping git clone, nix will fetch on demand
Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.349072Z DEBUG ThreadId(51) builder::evaluator::flake: evaluating flake attribute expr=(builtins.getFlake "git+https://github.com/wavelens/gradient?rev=900a095e2b1ab94fd5cf9338cf9128e72236151e").checks.x86_64-linux
Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.349406Z DEBUG ThreadId(51) builder::evaluator::flake: Skipping attribute path not present in flake path=checks.x86_64-linux error=nix eval failed
Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.349427Z DEBUG ThreadId(51) builder::evaluator::flake: evaluating flake attribute expr=(builtins.getFlake "git+https://github.com/wavelens/gradient?rev=900a095e2b1ab94fd5cf9338cf9128e72236151e").checks.x86_64-linux
Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.349761Z DEBUG ThreadId(51) builder::evaluator::flake: Skipping attribute path not present in flake path=checks.x86_64-linux error=nix eval failed
Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.349786Z ERROR ThreadId(51) builder::evaluator::flake: Failed to get partial derivations for key: checks.x86_64-linux
Apr 07 02:19:20 hydra gradient-server[3983247]: 2026-04-07T00:19:20.349867Z  WARN ThreadId(21) schedule_evaluation{evaluation=Model { id: 43951954-036c-46cf-aeed-1e8209d7a347, project: Some(0f5ae8b9-567e-4dbe-80c1-5237c0b6f62a), repository: "https://github.com/wavelens/gradient", commit: f4bf55f5-df19-48b5-a470-edf3a9e1a7ef, wildcard: "checks.x86_64-linux.*", status: Queued, previous: Some(3a6af860-b703-422d-aeb7-0d5039134896), next: None, created_at: 2026-04-07T00:19:10.379501, updated_at: 2026-04-07T00:19:10.379501, error: None } evaluation_id=43951954-036c-46cf-aeed-1e8209d7a347}:evaluate{evaluation=Model { id: 43951954-036c-46cf-aeed-1e8209d7a347, project: Some(0f5ae8b9-567e-4dbe-80c1-5237c0b6f62a), repository: "https://github.com/wavelens/gradient", commit: f4bf55f5-df19-48b5-a470-edf3a9e1a7ef, wildcard: "checks.x86_64-linux.*", status: Queued, previous: Some(3a6af860-b703-422d-aeb7-0d5039134896), next: None, created_at: 2026-04-07T00:19:10.379501, updated_at: 2026-04-07T00:19:10.379501, error: None } evaluation_id=43951954-036c-46cf-aeed-1e8209d7a347}: builder::evaluator: No derivations found for evaluation

but it works with the command, why?

Long-lived worker pool over a pipe (fast, slightly more code)

  Spawn N persistent eval-worker children at startup. Each speaks a tiny line-delimited JSON protocol over stdin/stdout. The parent has a Pool<EvalWorker> (just
  like NixStorePool) with a semaphore. A worker keeps its NixEvaluator alive across calls, so the libnix init cost is paid once.

  Same isolation benefits as #1 plus amortized startup cost. This is what hydra-queue-runner and nix-eval-jobs do internally.

  Pros: ~free per call after warmup, true parallelism scaled by worker count, still process-isolated.
  Cons: need a tiny IPC protocol; need to handle worker death (respawn on EOF/exit).
