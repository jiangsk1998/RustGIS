# 监测图斑地类边界分割

从影像纹理与颜色切分图斑内地块边界；不判定地类、不降分辨率。
外环＝图斑原始边界；内部界线为平滑曲线。图斑层以 JCBH 标识。

## 步骤 0：原生底板与体检

- 只用**带 EPSG 的 1 m 栅格**做底板。
- 源是无 CRS 的 COG/GeoTIFF 时：在转换器导出时写入 CRS
  （CGCS2000 3°带 39 → EPSG:4527；交付坐标带号 39＋东伪 500 km 已实证）。
  **勿依赖 `geolibre:assign_projection_raster`**：已知 bug 会丢 geotransform
   （输出范围变成全球 [-180,90,180,90]）；若用过，必须先查输出 extent 是否正常。
- 大图幅整幅层**默认读概览（≈4 m），显示与统计皆然，无用户开关**。
  一切分析只在"小裁剪块"上进行。
- **原生体检**：对图斑裁剪后立即分区统计，**面积(m²) ≈ 像元数（±10%）**才准继续。
  反例：331.75 m² 只摊 15 px＝概览污染，判定失败，停止。

## 每个图斑（先选中该图斑）

1. `geolibre:clip_raster_to_polygon` — 以**外扩 15 m 的缓冲面**裁剪（给分割上下文；
   输出须 ≈1 px/m，尺寸约 (长+30)×(宽+30) px）。
2. `geolibre:image_segmentation` — input=①，threshold=2.5，min_area=200。
   反馈"太碎"→上调 threshold；"太粗"→下调。
3. `geolibre:segments_merge_small_regions` — segments=②，min_size=30，method=nearest
   （<30 m² 碎块并入最近邻）。
4. `geolibre:boundary_clean` — input=③，method=majority，iterations=1。
5. **回裁图斑**（新增）：`geolibre:clip_raster_to_polygon` — input=④，
   polygons=图斑层本体（无缓冲）——外环在此回到图斑边界。
6. `geolibre:raster_to_vector_polygons` — input=⑤。
7. `geolibre:smooth_shared_edges` — input=⑥，algorithm=paek，tolerance=4，
   snap_tolerance=1，smooth_boundary=false（外环不平滑，只平滑内部共用边）。
8. `geolibre:align_features` — input=⑦，target=图斑层，search_distance=1.5
   （外环锯齿顶点吸附到图斑精确边界）。
9. `spatialJoin` — target=⑧，join=图斑层，match="Largest overlap"。
10. `calculateGeometry` — input=⑨，用其 area_km2 报面积。

## 验收（全部满足才算过）

- 像元体检（步骤 0）：面积 m² ≈ 像元数 ±10%。
- 面积：分割并集 vs 图斑 Shape_Area 差 <1%。
- 碎斑：合并+回裁后 <30 m² 的面数 = 0。
- `geolibre:smooth_shared_edges` 报告 "Collapsed ring count: 0"。
- 无自相交。

## 批量与汇总

- **样板先行**：首斑全流程+验收通过后，才批量推广。
- 逐图斑重复 10 步；完成后 `merge` 成"地块分割成果"层。
- 统计（zonalStats 等）**只准在裁剪小块上跑**；整幅统计读概览、数值作废。
- 目视核对时裁剪块颜色若与整幅不一致：`raster.stretch` 用 `like`=整幅层对齐拉伸
  ——那是显示归一化差异，不是数据差异。

## 禁止

- 不用 `geolibre:intersect`（会把面切成毫米碎块）；回裁已在栅格步骤完成掩膜。
- 面积不用 `$area` 或 GeoLibre 面积工具（不可靠），用 calculateGeometry 的 area_km2。
- 不在无 EPSG 的栅格上直接裁剪/统计（报 CRS 错或静默读概览）。
- 不用 extract_raster_values_at_points（输出 EPSG:0，已弃用）。