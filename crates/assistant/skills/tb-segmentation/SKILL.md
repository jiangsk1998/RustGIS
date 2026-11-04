---
name: tb-segmentation
description: Segment monitoring parcels into land-class boundaries from imagery (region growing, small-region merging, boundary regularisation): the outer ring follows the parcel, inner lines are smooth. Use for "图斑分割", "切地块", "地类边界", parcel segmentation or land-class boundaries.
---

# Segmenting monitoring parcels into land-class boundaries

Cut plot boundaries inside a parcel from the imagery's texture and colour only: this doesn't
assign land classes and doesn't reduce resolution. The outer ring is the parcel's original
boundary; the inner boundaries are smooth curves.

## Before starting

- The image raster needs an EPSG code in its header (CGCS2000 3-degree zone 39 → EPSG 4527). For
  one without it, write the code with GDAL, which keeps the geotransform and overviews:
  `gdal_edit.py -a_srs EPSG:4527 image.tif`. Don't use `geolibre:assign_projection_raster`: it
  drops the geotransform.
- A large local COG added as a tile layer (Add Data read it in place) is used by these tools by
  its layer name: the tools read the file from its path.
- The parcel layer has `JCBH`.
- Baseline parameters: threshold 2.5 / min_area 200 / small regions 30 m² / PAEK 4 m.
  Feedback "too fragmented" ("太碎") → raise the threshold; "too coarse" ("太粗") → lower it.

## Per parcel (select the parcel first)

1. `geolibre:clip_raster_to_polygon` — clip the image to the selected parcel (NoData outside it).
2. `geolibre:image_segmentation` — inputs=the clipped raster (pass the multi-band layer name as
   it is), threshold=2.5, min_area=200.
3. `geolibre:segments_merge_small_regions` — segments=②, min_size=30, method=nearest (fragments
   under 30 m² join their nearest neighbour).
4. `geolibre:boundary_clean` — input=③, method=majority, iterations=1.
5. `geolibre:raster_to_vector_polygons` — input=④.
6. `geolibre:smooth_shared_edges` — input=⑤, algorithm=paek, tolerance=4, snap_tolerance=1,
   smooth_boundary=false (the outer ring isn't smoothed; only inner shared edges are).
7. `geolibre:align_features` — input=⑥, target=the parcel layer, search_distance=1.5 (snap the
   outer ring's jagged vertices to the parcel's exact boundary).
8. `spatialJoin` — target=⑦, join=the parcel layer, match="Largest overlap".
9. `calculateGeometry` — input=⑧ (use its area_km2 for area).

## Acceptance

- Compare the total area_km2 from `read_features` with the parcel's `Shape_Area`: the
  difference should be <1%.
- `geolibre:smooth_shared_edges` reports "Collapsed ring count: 0".

## Batch and summary

- Repeat the 9 steps for each parcel; when done, use `merge` to combine them into one "parcel
  segmentation result" layer.
- Style: `layer.noFill` + `layer.outline` {"color":"#FFFF00","width":2}.

## Don't

- Don't use `geolibre:intersect` (it cuts polygons into millimetre-sized fragments); the clip
  already carries the mask, so no intersection is needed.
- GeoLibre output field names don't support Chinese; for area don't use `$area` or GeoLibre's
  area tools (their values aren't reliable).
