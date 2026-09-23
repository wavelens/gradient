# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""Turn a completed evaluation's report into a gradient-daemon store-spec.nix
that replays its graph: build edges, outputs, references, sizes and durations."""

from __future__ import annotations

import json
import re
import sqlite3
import sys
from datetime import datetime

from .commands import BUILD_STATUS, EVALUATION_STATUS

BUILDTIME = 0
BUILD_COMPLETED = next(k for k, v in BUILD_STATUS.items() if v == "Completed")
LEAF_SIZE = 64


class NotCompleted(Exception):
    """The report does not describe a finished, untruncated evaluation."""


def warn(message: str) -> None:
    print(f"store-spec: {message}", file=sys.stderr)


def check_completed(conn: sqlite3.Connection) -> None:
    row = conn.execute("SELECT status FROM evaluation").fetchone()
    status = EVALUATION_STATUS.get(int(row[0])) if row else None
    if status != "Completed":
        raise NotCompleted(f"evaluation status is {status or 'missing'}, not Completed")

    truncated = conn.execute(
        """SELECT "table" FROM report_manifest
           WHERE "table" IN ('derivation', 'derivation_output') AND rows_included < rows_available"""
    ).fetchall()
    if truncated:
        raise NotCompleted(f"report truncated: {[t[0] for t in truncated]}")


def node_keys(rows) -> dict[str, str]:
    keys: dict[str, str] = {}
    taken: set[str] = set()
    for drv_id, name, drv_hash in rows:
        base = re.sub(r"[^A-Za-z0-9_.+-]", "-", name or "drv")
        key = base if base not in taken else f"{base}-{(drv_hash or drv_id)[:8]}"
        taken.add(key)
        keys[drv_id] = key
    return keys


def duration_ms(conn: sqlite3.Connection, drv_id: str) -> float | None:
    row = conn.execute(
        """SELECT a.build_started_at, a.build_finished_at FROM build_attempt a
           JOIN derivation_build b ON b.id = a.derivation_build
           WHERE b.derivation = ? AND b.status = ? AND a.substitute = 0
             AND a.build_started_at IS NOT NULL AND a.build_finished_at IS NOT NULL
           ORDER BY a.build_finished_at DESC LIMIT 1""",
        (drv_id, BUILD_COMPLETED),
    ).fetchone()
    if not row:
        return None
    start, end = (datetime.fromisoformat(v) for v in row)
    return (end - start).total_seconds() * 1000


def reaches(nodes: dict, start: str, target: str) -> bool:
    stack, seen = [start], set()
    while stack:
        key = stack.pop()
        if key == target:
            return True
        if key not in seen:
            seen.add(key)
            stack.extend(nodes[key]["deps"])
    return False


def new_node(key: str, *, fixed_output=False, prefer_local=False, allow_subst=True) -> dict:
    return {
        "name": key,
        "deps": [],
        "fixedOutput": fixed_output,
        "preferLocalBuild": prefer_local,
        "allowSubstitutes": allow_subst,
        "outputs": {},
        "build": {"durationMs": None},
        "present": {"workers": [], "cache": False},
    }


class Graph:
    def __init__(self, nodes: dict, path_owner: dict, max_size: int):
        self.nodes = nodes
        self.path_owner = path_owner
        self.leaf_size = min(LEAF_SIZE, max_size)

    def owner_of(self, store_path: str) -> tuple[str, str]:
        ref_hash = store_path.rsplit("/", 1)[-1].split("-", 1)[0]
        if ref_hash in self.path_owner:
            return self.path_owner[ref_hash]
        key = f"x-{ref_hash[:12]}"
        if key not in self.nodes:
            leaf = new_node(key)
            leaf["outputs"]["out"] = {"size": self.leaf_size, "references": []}
            leaf["present"]["cache"] = True
            self.nodes[key] = leaf
        self.path_owner[ref_hash] = (key, "out")
        return key, "out"

    def reference(self, key: str, store_path: str) -> str | None:
        owner, output = self.owner_of(store_path)
        if owner == key:
            return None
        node = self.nodes[key]
        if owner not in node["deps"]:
            if reaches(self.nodes, owner, key):
                warn(f"dropping reference {key} -> {owner}: it would close a cycle")
                return None
            node["deps"].append(owner)
        return f"{owner}.{output}"

    def close_cache(self) -> None:
        pending = [k for k, n in self.nodes.items() if n["present"]["cache"]]
        while pending:
            node = self.nodes[pending.pop()]
            for out in node["outputs"].values():
                for ref in out["references"]:
                    target = self.nodes[ref.split(".", 1)[0]]
                    if not target["present"]["cache"]:
                        target["present"]["cache"] = True
                        target["build"]["durationMs"] = None
                        pending.append(target["name"])


def from_report(conn, *, name, time_scale, max_size, random_durations, workers) -> dict:
    check_completed(conn)
    rows = conn.execute(
        """SELECT id, name, hash, is_fixed_output, prefer_local_build, allow_substitutes
           FROM derivation"""
    ).fetchall()
    keys = node_keys([(r[0], r[1], r[2]) for r in rows])
    nodes = {}
    for drv_id, _, _, fod, local, subst in rows:
        node = new_node(keys[drv_id], fixed_output=bool(fod), prefer_local=bool(local), allow_subst=bool(subst))
        ms = None if random_durations else duration_ms(conn, drv_id)
        node["build"]["durationMs"] = None if ms is None else int(ms * time_scale)
        nodes[keys[drv_id]] = node

    outputs = conn.execute(
        "SELECT derivation, name, hash, nar_size, is_cached, references_list FROM derivation_output"
    ).fetchall()
    graph = Graph(nodes, {o[2]: (keys[o[0]], o[1]) for o in outputs if o[0] in keys}, max_size)

    edges = conn.execute(
        "SELECT derivation, dependency FROM derivation_dependency WHERE kind = ?", (BUILDTIME,)
    )
    for drv_id, dep_id in edges:
        if drv_id not in keys:
            continue
        if dep_id not in keys:
            warn(f"{keys[drv_id]} depends on {dep_id}, which the report does not carry")
            continue
        nodes[keys[drv_id]]["deps"].append(keys[dep_id])

    for drv_id, out_name, _, nar_size, cached, refs in outputs:
        if drv_id not in keys:
            continue
        key = keys[drv_id]
        references = [graph.reference(key, p) for p in (refs or "").split()]
        nodes[key]["outputs"][out_name] = {
            "size": min(int(nar_size or 0), max_size),
            "references": [r for r in references if r],
        }
        if cached:
            nodes[key]["present"]["cache"] = True
            nodes[key]["build"]["durationMs"] = None

    graph.close_cache()
    for node in nodes.values():
        if node["present"]["cache"]:
            node["present"]["workers"] = list(workers)

    entry = [keys[r[0]] for r in conn.execute("SELECT derivation FROM entry_point") if r[0] in keys]
    return {"name": name, "timing": {"scale": 1.0}, "entryPoints": entry, "derivations": nodes}


NIX_KEYWORDS = {"assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with"}


def nix_key(key: str) -> str:
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_'-]*", key) and key not in NIX_KEYWORDS:
        return key
    return json.dumps(key, ensure_ascii=False).replace("${", "\\${")


def nix(value, indent: int = 0) -> str:
    pad = "  " * indent
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False).replace("${", "\\${")
    if isinstance(value, list):
        return "[ " + "".join(nix(v, indent) + " " for v in value) + "]"
    inner = "".join(
        f"{pad}  {nix_key(k)} = {nix(v, indent + 1)};\n" for k, v in value.items()
    )
    return "{\n" + inner + pad + "}"


def render_nix(spec: dict) -> str:
    return "# Generated by `gradient-report store-spec`; regenerate from the report, do not edit.\n" + nix(spec) + "\n"
