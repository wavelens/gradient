# Overview

Gradient organises CI around **projects**. A project owns **tasks**, each task turns a flake into **evaluations**, and each evaluation fans out into **builds** that run on **workers** and end up in **caches**.

```mermaid
flowchart LR
    subgraph project[Project]
        task[Task] --> evaluation[Evaluation]
    end
    trigger([Trigger]) --> task
    evaluation --> build[Build]
    other[Evaluation of another project] --> build
    build --> worker[Worker]
    worker --> cache[(Cache)]
    evaluation --> action([Action])
```

## Building blocks

| Concept | Role |
|---|---|
| [Project](projects-and-tasks.md#project) | Unit of access: members, roles, workers and cache subscriptions |
| [Task](projects-and-tasks.md#task) | One repository plus the flake outputs to build, selected by a wildcard |
| Trigger | Starts an evaluation of a task: push, pull request, polling or schedule |
| Evaluation | One run of a task at one commit, listing every derivation to build |
| Build | One derivation, built once and shared by every evaluation that needs the same derivation |
| Worker | A machine that evaluates flakes and builds derivations for the projects that enable the worker |
| Cache | A Nix binary cache that stores build outputs and serves them to `nix` |
| Action | Reacts to evaluation and build events: mail, web request, forge status, pull request |

## Shared builds

A derivation is built once across the whole instance. Two projects that depend on the same derivation share one build: the first evaluation to reach the derivation dispatches the build, the others wait for the same result.

## Related

- [First project](../get-started/first-project.md): create each of these in the UI
