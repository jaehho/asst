#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["shapely>=2", "svgelements>=1.9", "numpy"]
# ///
"""Bundle Lucide icons (ISC) as GTK symbolic icons.

Lucide draws with strokes, and GTK recolors a symbolic icon by filling
its shapes, which turns a stroked outline into a blob. Each icon is
fetched from a pinned lucide-static release, its strokes are turned into
filled outlines, and the result is written as
`lucide-NAME-symbolic.svg` with a `<file>` line for the gresource.

    scripts/lucide-icons.py

adds them to crates/asst-gtk/icons/scalable/actions/ (GTK finds a
resource icon only in a context folder), lists them in the lucide block
of crates/asst-gtk/icons/asst.gresource.xml and in lucide-names.txt.
"""

import sys
import tempfile
import urllib.request
from pathlib import Path

import numpy as np
from shapely.geometry import LinearRing, LineString, Polygon
from shapely.ops import unary_union
from svgelements import SVG, Shape
from svgelements import Path as SvgPath

VERSION = "0.468.0"
URL = f"https://cdn.jsdelivr.net/npm/lucide-static@{VERSION}/icons/{{}}.svg"

# What a project or an area of life gets called, roughly in that order.
NAMES = [
    "house", "briefcase", "book-open", "graduation-cap", "school",
    "building-2", "landmark", "code", "terminal", "laptop", "server",
    "database", "git-branch", "bug", "wrench", "hammer", "cpu",
    "smartphone", "mail", "phone", "message-circle", "calendar", "clock",
    "alarm-clock", "shopping-cart", "shopping-bag", "wallet", "banknote",
    "credit-card", "gift", "package", "heart", "star", "flag", "bookmark",
    "tag", "lightbulb", "target", "trophy", "rocket", "zap", "flame",
    "sun", "moon", "cloud", "umbrella", "snowflake", "leaf", "trees",
    "flower-2", "sprout", "mountain", "car", "bus", "bike", "plane",
    "train-front", "ship", "map-pin", "globe", "tent", "compass",
    "utensils", "coffee", "wine", "pizza", "apple", "carrot", "dumbbell",
    "activity", "stethoscope", "pill", "baby", "paw-print", "music",
    "headphones", "film", "tv", "gamepad-2", "camera", "palette", "brush",
    "pencil", "scissors", "users", "user", "smile", "folder", "inbox",
    "archive", "file-text", "notebook", "flask-conical", "microscope",
    "atom", "calculator", "key", "lock", "shield", "sparkles",
    "wand-sparkles", "puzzle",
]  # fmt: skip

OUT = Path(__file__).resolve().parent.parent / "crates/asst-gtk/icons"
STROKE = 2.0
# Fewest points that make a line, a polygon and a ring.
LINE, POLYGON, RING = 2, 4, 4


def geometry(el):
    """The region an element paints, in icon units."""
    fill = el.values.get("fill", "none")
    stroke = el.values.get("stroke", "currentColor")
    path = el if isinstance(el, SvgPath) else SvgPath(el)
    parts = []
    for raw in path.as_subpaths():
        sub = SvgPath(raw)
        n = max(12, int(sub.length(error=1e-4) / 0.1))
        pts = [tuple(p) for p in sub.npoint(np.linspace(0, 1, n))]
        if len(pts) < LINE:
            continue
        closed = np.allclose(pts[0], pts[-1], atol=1e-3)
        if fill not in ("none", None, "") and len(pts) >= POLYGON:
            parts.append(Polygon(pts).buffer(0))
        if stroke not in ("none", None, ""):
            line = (
                LinearRing(pts[:-1])
                if closed and len(pts) >= RING
                else LineString(pts)
            )
            parts.append(
                line.buffer(
                    STROKE / 2,
                    quad_segs=8,
                    cap_style="round",
                    join_style="round",
                )
            )
    return parts


def convert(name):
    with urllib.request.urlopen(URL.format(name), timeout=30) as r:
        data = r.read()
    with tempfile.NamedTemporaryFile(suffix=".svg") as tmp:
        tmp.write(data)
        tmp.flush()
        svg = SVG.parse(tmp.name)
    parts = [
        g
        for el in svg.elements()
        if isinstance(el, Shape) and not isinstance(el, SVG)
        for g in geometry(el)
    ]
    shape = unary_union(parts).simplify(0.01)
    polys = [shape] if shape.geom_type == "Polygon" else list(shape.geoms)
    d = []
    for poly in polys:
        for ring in [poly.exterior, *poly.interiors]:
            pts = [f"{x:.2f} {y:.2f}" for x, y in list(ring.coords)[:-1]]
            d.append(f"M{pts[0]}L" + " ".join(pts[1:]) + "Z")
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" '
        'viewBox="0 0 24 24"><path fill-rule="evenodd" d="'
        + "".join(d)
        + '"/></svg>\n'
    )


def main():
    folder = OUT / "scalable" / "actions"
    folder.mkdir(parents=True, exist_ok=True)
    made = []
    for name in NAMES:
        try:
            svg = convert(name)
        except Exception as e:  # a renamed icon should not stop the rest
            print(f"skipped {name}: {e}", file=sys.stderr)
            continue
        (folder / f"lucide-{name}-symbolic.svg").write_text(svg)
        made.append(name)
    xml = OUT / "asst.gresource.xml"
    text = xml.read_text()
    head, rest = text.split("<!-- lucide:begin", 1)
    tail = rest.split("<!-- lucide:end -->", 1)[1]
    files = "".join(
        f"    <file>scalable/actions/lucide-{n}-symbolic.svg</file>\n"
        for n in made
    )
    xml.write_text(
        head
        + "<!-- lucide:begin (scripts/lucide-icons.py) -->\n"
        + files
        + "    <!-- lucide:end -->"
        + tail
    )
    (OUT / "lucide-names.txt").write_text("\n".join(made) + "\n")
    print(f"{len(made)} icons from lucide-static {VERSION}")


if __name__ == "__main__":
    main()
