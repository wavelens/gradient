import sys

import pathops
from picosvg.svg import SVG
from picosvg.svg_pathops import skia_path, svg_commands
from picosvg.svg_types import SVGPath

DARK_MODE = "@media (prefers-color-scheme: dark) { path { fill: #fff } }"


# Typst hides the layers behind the front lambda by painting white over them, so
# the shapes are replayed in paint order: white cuts the ink below it, black adds.
def visible_ink(svg):
    ink = pathops.Path()
    for shape in svg.topicosvg().shapes():
        op = pathops.PathOp.DIFFERENCE if shape.fill == "#ffffff" else pathops.PathOp.UNION
        ink = pathops.op(ink, skia_path(shape.as_cmd_seq(), shape.fill_rule), op, fix_winding=True)
    return ink


def render(ink):
    left, top, right, bottom = ink.bounds
    d = SVGPath.from_commands(svg_commands(ink)).round_floats(3).d
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{left:.3f} {top:.3f} {right - left:.3f} {bottom - top:.3f}">'
        f"<style>{DARK_MODE}</style>"
        f'<path d="{d}"/>'
        "</svg>\n"
    )


def main(source, target):
    with open(target, "w") as out:
        out.write(render(visible_ink(SVG.parse(source))))


if __name__ == "__main__":
    main(*sys.argv[1:])
