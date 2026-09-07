# MCP Server

`gradient mcp` serves your Gradient instance to any [Model Context
Protocol](https://modelcontextprotocol.io) client over stdio, so an assistant
can read your CI: which evaluations ran, which builds failed, and what the
failing derivation printed.

Access is read-only. The server exposes no tool that dispatches an evaluation,
edits a project, or aborts a build.

## Setup

The MCP server reuses the CLI's own credentials, so logging in is the entire
setup:

```sh
gradient login https://gradient.example.com
```

That stores the server URL and auth token in `config.toml` (see
[CLI](cli.md#configuration)); every MCP tool call is made as that user and is
limited to the projects they belong to. If a project is selected with
`gradient project select <name>` it becomes the default for tools that take a
`project` argument.

Point your client at the binary. For Claude Code:

```sh
claude mcp add gradient -- gradient mcp
```

For a client configured through JSON:

```json
{
  "mcpServers": {
    "gradient": {
      "command": "gradient",
      "args": ["mcp"]
    }
  }
}
```

## Tools

| Tool | Returns |
|---|---|
| `list_projects` | Projects the authenticated user belongs to |
| `list_tasks` | Tasks of a project |
| `list_evaluations` | A task's evaluations with their build status counts |
| `get_evaluation` | One evaluation: commit, status, and error if it failed |
| `list_builds` | The builds of an evaluation |
| `get_build` | One build: status, derivation path, architecture, outputs |
| `get_build_log` | A build's log, whole or by line range |
| `search_build_log` | Matching log lines with their line numbers |

The usual path from a red pipeline to a cause is `list_evaluations` on the task,
`list_builds` on the failed evaluation, then `get_build_log` on the build that
failed. For long logs, `search_build_log` finds the line number to read around.

## Troubleshooting

`gradient mcp` exits immediately with `Server URL not set` when no server is
configured; run `gradient login` first.

The command speaks JSON-RPC on stdout and writes everything else to stderr, so
do not pipe stderr into stdout when wrapping it in a launcher script.
