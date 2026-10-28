# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import pathlib
import re
import sys

BACKEND = pathlib.Path(__file__).resolve().parents[2] / "backend"

SERVER_SUB_GROUPS = {
    "": ["frontend"],
    "database": ["cache", "web"],
    "email": ["smtp", "from"],
    "eval": ["cache"],
    "log": ["level"],
    "metrics": ["retention", "otlp"],
    "proto": ["anonymous_cache"],
}
WORKER_SUB_GROUPS = {"eval": ["cache"], "log": ["level"], "exec": ["allow", "defaults", "sops"]}
TYPES = [
    (r"16 bit unsigned integer; between 0 and 65535 \(both inclusive\)", "port"),
    (r"positive integer, meaning >0", "positive int"),
    (r"unsigned integer, meaning >=0", "unsigned int"),
    (r"signed integer", "int"),
    (r"integer between (\d+) and (\d+) \(both inclusive\)", r"int from \1 to \2"),
    (r"absolute path", "path"),
    (r"boolean", "bool"),
]


def camel(name):
    head, *rest = name.split("_")
    return head + "".join(part.capitalize() for part in rest)


def path_of(spec, sub_groups, prefix):
    group = [camel(spec["group"])] if spec["group"] else []
    for sub in sub_groups.get(spec["group"], []):
        if spec["field"].startswith(sub + "_"):
            return ".".join(prefix + group + [camel(sub), camel(spec["field"][len(sub) + 1:])])
    return ".".join(prefix + group + [camel(spec["field"])])


def type_of(text):
    for pattern, short in TYPES:
        text = re.sub(pattern, short, text)
    text = re.sub(r'one of ((?:"[^"]*"(?:, )?)+)', lambda match: "one of " + " ".join(f"`{value}`" for value in re.findall(r'"([^"]*)"', match.group(1))), text)
    return text.replace("|", "\\|")


def default_of(text):
    if text is None:
        return "-"
    text = re.sub(r"\s+", " ", text).strip().replace("config.services.gradient.", "")
    if re.search(r"pkgs\.|lib\.|\$\{|config\.", text):
        return "derived"
    return f"`{text}`"


def description_of(text):
    text = re.sub(r"\{(?:option|command|file|env|var)\}`([^`]*)`", r"`\1`", text)
    text = text.replace("services.gradient.", "")
    return re.sub(r"\s+", " ", text).strip().replace("|", "\\|")


def section_of(name):
    parts = name.split(".")
    if len(parts) == 1:
        return "General"
    if parts[0] == "worker" and len(parts) > 2:
        return "`worker." + parts[1] + "`"
    return f"`{parts[0]}`"


def render(options, server_specs, worker_specs):
    env = {path_of(spec, SERVER_SUB_GROUPS, []): spec["env"] for spec in server_specs}
    env.update({path_of(spec, WORKER_SUB_GROUPS, ["worker"]): spec["env"] for spec in worker_specs})
    sections = {}
    for option in options:
        if option["name"].split(".")[0] == "state" or option["type"] == "submodule":
            continue
        sections.setdefault(section_of(option["name"]), []).append(option)
    lines = []
    for title in sorted(sections, key=lambda title: (title != "General", title.strip("`"))):
        lines += [f"## {title}", "", "| Option | Type | Default | Env | Description |", "|---|---|---|---|---|"]
        for option in sorted(sections[title], key=lambda option: option["name"]):
            name = env.get(option["name"])
            lines.append(
                f"| `{option['name']}` | {type_of(option['type'])} | {default_of(option['default'])} | "
                f"{f'`{name}`' if name else '-'} | {description_of(option['description'])} |"
            )
        lines.append("")
    return "\n".join(lines).rstrip("\n") + "\n"


if __name__ == "__main__":
    server = json.loads((BACKEND / "gradient-types" / "options.json").read_text())
    worker = json.loads((BACKEND / "gradient-worker" / "options.json").read_text())
    sys.stdout.write(render(json.load(sys.stdin), server, worker))
