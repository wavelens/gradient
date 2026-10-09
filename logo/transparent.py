import sys

import pathops
from picosvg.svg import SVG
from picosvg.svg_pathops import skia_path, svg_commands
from picosvg.svg_types import SVGPath

DARK_MODE = "@media (prefers-color-scheme: dark) { path { fill: #fff } }"
OUTLINE_WIDTH = 3.0


# Typst hides the layers behind the front lambda by painting white over them, so
# the shapes are replayed in paint order: white cuts the ink below it, black adds.
def visible_ink(svg):
    ink = pathops.Path()
    for shape in svg.topicosvg().shapes():
        op = pathops.PathOp.DIFFERENCE if shape.fill == "#ffffff" else pathops.PathOp.UNION
        ink = pathops.op(ink, skia_path(shape.as_cmd_seq(), shape.fill_rule), op, fix_winding=True)
    return ink


def outline(ink):
    halo = skia_path(svg_commands(ink), "nonzero")
    halo.stroke(OUTLINE_WIDTH, pathops.LineCap.ROUND_CAP, pathops.LineJoin.ROUND_JOIN, 4)
    return pathops.op(halo, ink, pathops.PathOp.UNION, fix_winding=True)


def path_data(path):
    return SVGPath.from_commands(svg_commands(path)).round_floats(3).d


def render(bounds, body):
    left, top, right, bottom = bounds
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{left:.3f} {top:.3f} {right - left:.3f} {bottom - top:.3f}">'
        f"{body}</svg>\n"
    )


def plain(ink):
    return render(ink.bounds, f'<style>{DARK_MODE}</style><path d="{path_data(ink)}"/>')


def outlined(ink):
    halo = outline(ink)
    return render(halo.bounds, f'<path fill="#fff" d="{path_data(halo)}"/><path d="{path_data(ink)}"/>')


def main(source, plain_target, outlined_target):
    ink = visible_ink(SVG.parse(source))
    for target, variant in ((plain_target, plain), (outlined_target, outlined)):
        with open(target, "w") as out:
            out.write(variant(ink))


if __name__ == "__main__":
    main(*sys.argv[1:])
