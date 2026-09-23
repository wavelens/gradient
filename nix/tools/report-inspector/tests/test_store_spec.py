# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only
"""A small graph a -> b -> c plus an output reference to `x`, a path the report
does not carry, built with the real export's column names for the tables the
generator reads."""

from __future__ import annotations

import sqlite3

import pytest

from gradient_report.__main__ import main
from gradient_report.db import SUPPORTED_SCHEMA, open_report
from gradient_report.store_spec import NotCompleted, from_report, render_nix

HASH = {name: name * 32 for name in "abcx"}


def path(name: str) -> str:
    return f"/nix/store/{HASH[name]}-{name}"


def graph_db(
    file,
    *,
    eval_status: int = 5,
    collision: bool = False,
    cyclic: bool = False,
    statuses: dict | None = None,
    jobs: str = "abc",
    names: dict | None = None,
) -> None:
    conn = sqlite3.connect(file)
    conn.executescript(
        """
        CREATE TABLE report_meta (schema_version INTEGER);
        CREATE TABLE report_manifest ("table" TEXT, rows_included INTEGER,
            rows_available INTEGER, filter TEXT, redactions TEXT);
        CREATE TABLE evaluation (id TEXT, status INTEGER);
        CREATE TABLE derivation (id TEXT, architecture TEXT, hash TEXT, name TEXT,
            prefer_local_build INTEGER, allow_substitutes INTEGER, is_fixed_output INTEGER);
        CREATE TABLE derivation_dependency (id TEXT, derivation TEXT, dependency TEXT, kind INTEGER);
        CREATE TABLE derivation_output (id TEXT, derivation TEXT, name TEXT, hash TEXT,
            nar_size INTEGER, is_cached INTEGER, references_list TEXT);
        CREATE TABLE derivation_build (id TEXT, derivation TEXT, status INTEGER);
        CREATE TABLE build_attempt (id TEXT, derivation_build TEXT, substitute INTEGER,
            build_started_at TEXT, build_finished_at TEXT);
        CREATE TABLE entry_point (id TEXT, derivation TEXT);
        CREATE TABLE build_job (id TEXT, evaluation TEXT, derivation TEXT, derivation_build TEXT);
        """
    )
    conn.execute("INSERT INTO report_meta VALUES (?)", (SUPPORTED_SCHEMA,))
    conn.execute("INSERT INTO evaluation VALUES ('e', ?)", (eval_status,))
    names = names or {"a": "source", "b": "source" if collision else "lib", "c": "app"}
    statuses = {"a": 3, "b": 3, "c": 3} | (statuses or {})
    for node in "abc":
        conn.execute(
            "INSERT INTO derivation VALUES (?, 'x86_64-linux', ?, ?, 0, 1, 0)",
            (f"d{node}", f"{node}drv{'0' * 28}", names[node]),
        )
        conn.execute(
            "INSERT INTO derivation_build VALUES (?, ?, ?)", (f"b{node}", f"d{node}", statuses[node])
        )
        if node in jobs:
            conn.execute("INSERT INTO build_job VALUES (?, 'e', ?, ?)", (f"j{node}", f"d{node}", f"b{node}"))
        conn.execute(
            "INSERT INTO build_attempt VALUES (?, ?, 0, '2026-09-01 10:00:00', '2026-09-01 10:00:02')",
            (f"a{node}", f"b{node}"),
        )

    conn.execute("INSERT INTO derivation_dependency VALUES ('e1', 'db', 'da', 0)")
    conn.execute("INSERT INTO derivation_dependency VALUES ('e2', 'dc', 'db', 0)")
    conn.execute("INSERT INTO derivation_dependency VALUES ('e3', 'dc', 'dx', 0)")
    references = {"a": path("c") if cyclic else "", "b": path("a"), "c": f"{path('x')} {path('c')}"}
    for node in "abc":
        conn.execute(
            "INSERT INTO derivation_output VALUES (?, ?, 'out', ?, 5000, 0, ?)",
            (f"o{node}", f"d{node}", HASH[node], references[node]),
        )

    conn.execute("INSERT INTO entry_point VALUES ('ep', 'dc')")
    conn.commit()
    conn.close()


def report(tmp_path, **kwargs) -> sqlite3.Connection:
    file = tmp_path / "r.db"
    graph_db(file, **kwargs)
    return open_report(file)


def spec_of(conn, **overrides):
    args = {
        "name": "t",
        "time_scale": 1.0,
        "max_size": 65536,
        "random_durations": False,
        "workers": [],
    }
    return from_report(conn, **(args | overrides))


def test_edges_outputs_and_references(tmp_path):
    spec = spec_of(report(tmp_path))
    d = spec["derivations"]
    assert d["lib"]["deps"] == ["source"]
    assert d["lib"]["outputs"]["out"]["references"] == ["source.out"]
    assert spec["entryPoints"] == ["app"]


def test_self_reference_is_not_an_edge(tmp_path):
    app = spec_of(report(tmp_path))["derivations"]["app"]
    assert "app.out" not in app["outputs"]["out"]["references"]
    assert "app" not in app["deps"]


def test_out_of_scope_reference_becomes_cached_leaf(tmp_path):
    spec = spec_of(report(tmp_path))
    leaves = {k: v for k, v in spec["derivations"].items() if k.startswith("x-")}
    assert len(leaves) == 1
    (key, leaf), = leaves.items()
    assert leaf["present"]["cache"] is True and leaf["deps"] == []
    app = spec["derivations"]["app"]
    assert f"{key}.out" in app["outputs"]["out"]["references"]
    assert key in app["deps"]


def test_durations_scale_and_random_mode(tmp_path):
    conn = report(tmp_path)
    fixed = spec_of(conn, time_scale=0.5)
    assert fixed["derivations"]["lib"]["build"]["durationMs"] == 1000
    rnd = spec_of(conn, random_durations=True)
    assert rnd["derivations"]["lib"]["build"]["durationMs"] is None


def test_sizes_are_capped(tmp_path):
    spec = spec_of(report(tmp_path), max_size=10)
    sizes = [o["size"] for n in spec["derivations"].values() for o in n["outputs"].values()]
    assert sizes and all(s <= 10 for s in sizes)


def test_name_collisions_get_suffixes(tmp_path):
    spec = spec_of(report(tmp_path, collision=True))
    assert len([k for k in spec["derivations"] if k.startswith("source")]) == 2


def test_refuses_incomplete_evaluation(tmp_path):
    with pytest.raises(NotCompleted):
        spec_of(report(tmp_path, eval_status=3))


def test_cyclic_reference_is_dropped_with_warning(tmp_path, capsys):
    spec = spec_of(report(tmp_path, cyclic=True))
    source = spec["derivations"]["source"]
    assert source["deps"] == []
    assert source["outputs"]["out"]["references"] == []
    assert "cycle" in capsys.readouterr().err


def test_workers_hold_only_cached_nodes(tmp_path):
    spec = spec_of(report(tmp_path), workers=["worker1"])
    present = {k: n["present"]["workers"] for k, n in spec["derivations"].items()}
    assert all(w == ["worker1"] for k, w in present.items() if k.startswith("x-"))
    assert present["app"] == []


def test_render_nix_round_trips_simple_values():
    text = render_nix(
        {"name": "t", "derivations": {"a": {"deps": [], "fixedOutput": False, "build": {"durationMs": None}}}}
    )
    assert "fixedOutput = false;" in text
    assert "durationMs = null;" in text
    assert "a = {" in text
    assert '"t/app" = 1;' in render_nix({"t/app": 1})


def test_render_nix_escapes_interpolation():
    assert '"\\${x}"' in render_nix({"log": "${x}"})


def test_cli_writes_the_file(tmp_path):
    graph_db(tmp_path / "r.db")
    out = tmp_path / "store-spec.nix"
    assert main([str(tmp_path / "r.db"), "store-spec", "-o", str(out), "--name", "replay"]) == 0
    assert 'name = "replay";' in out.read_text()


def test_substituted_and_previously_built_nodes_are_cached(tmp_path):
    spec = spec_of(report(tmp_path, statuses={"a": 7}, jobs="c"))
    d = spec["derivations"]
    assert d["source"]["present"]["cache"] is True
    assert d["lib"]["present"]["cache"] is True
    assert d["app"]["present"]["cache"] is False
    assert d["app"]["build"]["durationMs"] == 2000


def test_is_cached_after_the_run_does_not_make_a_built_node_present(tmp_path):
    file = tmp_path / "r.db"
    graph_db(file)
    with sqlite3.connect(file) as conn:
        conn.execute("UPDATE derivation_output SET is_cached = 1")
    spec = spec_of(open_report(file))
    assert not any(n["present"]["cache"] for k, n in spec["derivations"].items() if not k.startswith("x-"))


def test_failed_evaluation_needs_allow_failed_and_replays_the_failure(tmp_path):
    conn = report(tmp_path, eval_status=6, statuses={"b": 4, "c": 6})
    with pytest.raises(NotCompleted):
        spec_of(conn)
    d = spec_of(conn, allow_failed=True)["derivations"]
    assert d["lib"]["build"]["outcome"] == "fail"
    assert d["lib"]["build"]["failStatus"] == "PermanentFailure"
    assert "outcome" not in d["app"]["build"]


def test_keys_never_contain_dots(tmp_path):
    spec = spec_of(report(tmp_path, names={"a": "python3.14-x", "b": "lib", "c": "app"}))
    assert "python3_14-x" in spec["derivations"]
    assert spec["derivations"]["lib"]["outputs"]["out"]["references"] == ["python3_14-x.out"]


def test_anonymize_hides_names(tmp_path):
    spec = spec_of(report(tmp_path, names={"a": "secret-host", "b": "lib", "c": "app"}), anonymize=True)
    assert "secret" not in render_nix(spec)
    assert sorted(spec["derivations"]) == ["n0", "n1", "n2", "n3"]
