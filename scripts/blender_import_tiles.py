"""Import baked vr_fire terrain tiles into Blender, each placed at its real position.

Usage:
    blender --python scripts/blender_import_tiles.py
or open this file in Blender's Scripting tab and click Run.

Each tile's vertices are local to its NW corner, so tiles are offset by their EPSG:5070
bounds (from tiles/{tx}_{ty}.json) relative to the first tile's NW corner. After import,
Blender axes are X = east, Y = north, Z = elevation (m).
"""

import glob
import json
import os

import bpy

# Folder written by `vr_fire bake`. Defaults to <repo>/tiles next to this script.
TILES = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tiles")
LOD = 0  # 0 = 10 m, 1 = 30 m, 2 = 150 m, 3 = 750 m
Z_EXAGGERATION = 1.0  # try 2.0 to make relief easier to see

metas = sorted(glob.glob(os.path.join(TILES, "*.json")))
if not metas:
    raise SystemExit(f"No baked tiles in {TILES}; run `vr_fire bake` or edit TILES at the top of this script.")

origin = None
for path in metas:
    with open(path) as f:
        meta = json.load(f)
    b = meta["bounds"]
    if origin is None:  # floating origin = first tile's NW corner
        origin = (b["x_min"], b["y_max"])
    bpy.ops.import_scene.gltf(filepath=os.path.join(TILES, meta["lods"][LOD]["file"]))
    for obj in bpy.context.selected_objects:
        obj.location = (b["x_min"] - origin[0], b["y_max"] - origin[1], 0)
        obj.scale.z = Z_EXAGGERATION

# Default clip end is 1 km; a 3×3 block of tiles spans ~11 km.
for area in bpy.context.screen.areas:
    if area.type == "VIEW_3D":
        area.spaces[0].clip_end = 100000

print(f"Imported {len(metas)} tiles at LOD {LOD} from {TILES}")
