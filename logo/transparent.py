# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only

import sys

import pathops
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from picosvg.svg import SVG
from picosvg.svg_pathops import skia_path, svg_commands
from picosvg.svg_types import SVGPath

DARK_MODE = "@media (prefers-color-scheme: dark) { path { fill: #fff } }"
OUTLINE_WIDTH = 3.0
STICKER_OUTLINE_WIDTH = 5.0
STICKER_TEXT = "Gradient"
STICKER_TEXT_GAP = 3.0


# Typst hides the layers behind the front lambda by painting white over them, so
# the shapes are replayed in paint order: white cuts the ink below it, black adds.
def visible_ink(svg):
    ink = pathops.Path()
    for shape in svg.topicosvg().shapes():
        op = pathops.PathOp.DIFFERENCE if shape.fill == "#ffffff" else pathops.PathOp.UNION
        ink = pathops.op(ink, skia_path(shape.as_cmd_seq(), shape.fill_rule), op, fix_winding=True)
    return ink


def outline(ink, width=OUTLINE_WIDTH):
    halo = skia_path(svg_commands(ink), "nonzero")
    halo.stroke(width, pathops.LineCap.ROUND_CAP, pathops.LineJoin.ROUND_JOIN, 4)
    halo.convertConicsToQuads()
    return pathops.op(halo, ink, pathops.PathOp.UNION, fix_winding=True)


def text_outline(font, text):
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    pen = SVGPathPen(glyphs)
    advance = 0
    for char in text:
        name = cmap[ord(char)]
        glyphs[name].draw(TransformPen(pen, (1, 0, 0, -1, advance, 0)))
        advance += glyphs[name].width
    return skia_path(SVGPath(d=pen.getCommands()).as_cmd_seq(), "nonzero")


def caption(ink, font_path):
    text = text_outline(TTFont(font_path), STICKER_TEXT)
    left, top, right, _ = text.bounds
    ink_left, _, ink_right, ink_bottom = ink.bounds
    scale = (ink_right - ink_left) / (right - left)
    dx = ink_left - left * scale
    dy = ink_bottom + STICKER_TEXT_GAP - top * scale
    return text.transform(scaleX=scale, scaleY=scale, translateX=dx, translateY=dy)


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


def rectangle(bounds):
    left, top, right, bottom = bounds
    box = pathops.Path()
    box.moveTo(left, top)
    box.lineTo(right, top)
    box.lineTo(right, bottom)
    box.lineTo(left, bottom)
    box.close()
    return box


def sticker(ink, font_path):
    text = caption(ink, font_path)
    art = pathops.op(ink, text, pathops.PathOp.UNION, fix_winding=True)
    halo = pathops.op(
        outline(ink, STICKER_OUTLINE_WIDTH),
        outline(rectangle(text.bounds), STICKER_OUTLINE_WIDTH),
        pathops.PathOp.UNION,
        fix_winding=True,
    )
    return render(halo.bounds, f'<path fill="#fff" d="{path_data(halo)}"/><path d="{path_data(art)}"/>')


def main(source, font_path, plain_target, outlined_target, sticker_target):
    ink = visible_ink(SVG.parse(source))
    variants = (
        (plain_target, plain(ink)),
        (outlined_target, outlined(ink)),
        (sticker_target, sticker(ink, font_path)),
    )
    for target, svg in variants:
        with open(target, "w") as out:
            out.write(svg)


if __name__ == "__main__":
    main(*sys.argv[1:])
