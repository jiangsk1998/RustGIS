"""Regenerates the tables of `epsg_axes.rs`: which EPSG codes RustGIS knows are written north
(or latitude) first, and which it can't tell.

Usage (pyproj installed, from the repository root):

    python crates/data/src/raster/epsg_axes.py <crs-definitions src dir>

where the crs-definitions source is the crate's `src/` in the cargo registry, e.g.
`~/.cargo/registry/src/index.crates.io-*/crs-definitions-0.6.0/src`. It prints the two Rust
arrays; paste them over `NORTH_FIRST` and `UNKNOWN`, and update the versions in the module doc.

The rule: a code is north first when the first axis EPSG gives it is a northing or a latitude
(a southing-first one is unknown, below) and its PROJ string doesn't already put a north or
south axis first (`+axis=n..` or `+axis=s..`:
terrio-crs applies `+axis`, so its coordinates come out in EPSG's order). Unknown: the codes this
PROJ database lacks, and those EPSG writes southing first that a PROJ string without `+axis`
gives as easting and northing (a swap gives the pairing but not the signs).
"""

import re
import sys
from pathlib import Path

import pyproj
from pyproj.database import get_database_metadata

crsdefs = Path(sys.argv[1])
here = Path(__file__).resolve().parent

proj = {int(c): p for c, p in re.findall(r'^EPSG_\d+\|(\d+)\|"([^"]*)"', (crsdefs / "defs.rs").read_text(), re.M)}
proj.update({int(c): p for c, p in re.findall(r'^\s*\((\d+), "([^"]*)"\)', (here / "epsg_gap.rs").read_text(), re.M)})

north_first, unknown = [], []
for code in sorted(proj):
    try:
        crs = pyproj.CRS.from_epsg(code)
    except pyproj.exceptions.CRSError:
        unknown.append(code)
        continue
    if crs.is_geocentric or len(crs.axis_info) < 2:
        continue
    first = crs.axis_info[0].name.lower()
    epsg_ns = first.startswith(("northing", "southing", "geodetic latitude", "latitude"))
    axis = re.search(r"\+axis=([a-z]{2,3})", proj[code])
    out_ns = bool(axis) and axis.group(1)[0] in "ns"
    if epsg_ns and not out_ns:
        if first.startswith("southing"):
            unknown.append(code)
        else:
            north_first.append(code)


def rows(codes):
    return "\n".join("    " + ", ".join(str(c) for c in codes[i : i + 14]) + "," for i in range(0, len(codes), 14))


print(f"// PROJ {pyproj.proj_version_str}, EPSG {get_database_metadata('EPSG.VERSION')} "
      f"({get_database_metadata('EPSG.DATE')}), pyproj {pyproj.__version__}")
print(f"pub(crate) const NORTH_FIRST: &[u16] = &[\n{rows(north_first)}\n];\n")
print(f"pub(crate) const UNKNOWN: &[u16] = &[\n{rows(unknown)}\n];")
