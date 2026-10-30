# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import sys
from collections import defaultdict
from pathlib import Path

import tree_sitter
import tree_sitter_nix

PARSER = tree_sitter.Parser(tree_sitter.Language(tree_sitter_nix.language()))


def walk(node):
    yield node
    for child in node.children:
        yield from walk(child)


def attr_path(binding):
    path = binding.child_by_field_name("attrpath")
    return [attr.text.decode() for attr in path.children_by_field_name("attr")]


def repeated_keys(binding_set):
    paths_by_key = defaultdict(list)
    for binding in binding_set.children_by_field_name("binding"):
        if binding.type == "binding":
            path = attr_path(binding)
            paths_by_key[path[0]].append((binding.start_point.row + 1, ".".join(path)))
    for key, paths in paths_by_key.items():
        if len(paths) > 1:
            yield key, paths


def check_file(file):
    tree = PARSER.parse(file.read_bytes())
    found = False
    for node in walk(tree.root_node):
        if node.type != "binding_set":
            continue
        for key, paths in repeated_keys(node):
            found = True
            lines = ", ".join(f"{path} (line {row})" for row, path in paths)
            print(f"{file}: merge into `{key} = {{ ... }}`: {lines}")
    return found


def main():
    files = sorted(file for root in sys.argv[1:] for file in Path(root).rglob("*.nix"))
    failed = [file for file in files if check_file(file)]
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
