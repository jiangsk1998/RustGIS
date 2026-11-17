---
name: tb-segmentation
description: 按遥感影像对监测图斑做地类边界分割（区域生长+碎斑合并+边界规整），外圈贴合图斑边界、内线平滑，输出多边形地块。用于"图斑分割""切地块""地类边界"等请求。
---

# 监测图斑地类边界分割

只依据图斑覆盖区内的影像纹理/颜色切出地块边界，不做地类认定，不降分辨率。
外圈 = 图斑原始边界；内部分割线 = 平滑曲线。

## 前置
- 影像栅格须有 EPSG；无码坐标系先 geolibre:assign_projection_raster
  （CGCS2000 3度带39 → epsg 4527）。
- 图斑矢量层含 JCBH。
- 参数基线：threshold 2.5 / min_area 200 / 碎斑 30 ㎡ / PAEK 4 m。
  反馈"太碎"→调高 threshold；"太粗"→调低。

## 每图斑（先选中该图斑）
1. geolibre:clip_raster_to_polygon  影像按选中图斑裁剪（图斑外置 NoData）
2. geolibre:image_segmentation  inputs=裁剪影像（多波段层名直接传），threshold=2.5，min_area=200
3. geolibre:segments_merge_small_regions  segments=②，min_size=30，method=nearest（<30㎡碎斑就近并入邻段）
4. geolibre:boundary_clean  input=③，method=majority，iterations=1
5. geolibre:raster_to_vector_polygons  input=④
6. geolibre:smooth_shared_edges  input=⑤，algorithm=paek，tolerance=4，
   snap_tolerance=1，smooth_boundary=false（外圈不平滑，只平内部公共边）
7. geolibre:align_features  input=⑥，target=图斑矢量层，search_distance=1.5
   （外圈锯齿顶点吸附到图斑精确边界）
8. spatialJoin  target=⑦，join=图斑层，match="Largest overlap"
9. calculateGeometry  input=⑧（面积用其 area_km2）

## 验收
- read_features 合计 area_km2 对比图斑 Shape_Area，偏差应 <1%
- smooth_shared_edges 报告 "Collapsed ring count: 0"

## 批量与汇总
- 逐图斑循环上述 9 步；完成后用 merge 合并为一个"地块分割成果"层。
- 样式：layer.noFill + layer.outline {"color":"#FFFF00","width":2}

## 禁止
- 不用 geolibre:intersect（会把面切成毫米级碎块）；裁剪已含掩膜，无需相交。
- GeoLibre 输出字段名不支持中文；面积勿用 $area / geolibre 面积工具（数值不可靠）。