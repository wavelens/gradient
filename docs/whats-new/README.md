# What's New

Minor releases each have a file (`2.1.md`) with release notes for the "What's New!" dialog. Patch releases get no file.

```markdown
## Faster Dashboard
![Dashboard](2.1/dashboard.png)

Starred projects stay in the sidebar.

## Exec Jobs
icon: terminal

- Worker names instead of IDs.
- **Show job** in every exec job menu.
```

- `## Title` lines each open one dialog page.
- Screenshot lines `![alt](2.1/file.png)` show a 16:9 image from `2.1/` beside the file.
- `icon: <name>` entries show a Material Symbols icon on a page with no screenshot.
- Descriptions mix paragraphs, `-` lists, `**bold**`, `` `code` `` and links.

Execute `pnpm whats-new` in `frontend/` following each edit.
