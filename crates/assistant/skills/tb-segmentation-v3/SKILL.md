---
name: tb-segmentation-v3
description: 监测图斑内地类边界分割（v3 实战版）——从 1 m 影像切分地块边界：区域生长分割、小面合并、边界规整、共用边平滑、对齐图斑与面积验收。
---

# 监测图斑地类边界分割（v3）

从影像纹理与颜色切分图斑内地块边界；不判定地类、不降分辨率。
外环＝图斑原始边界；内部界线为平滑曲线。图斑层以 JCBH 标识。
规则括注来源：v2 批量经验（088→453 共 17 斑）＋ 128 收官案；全批 19 斑面数谱 1–22 面。

## 步骤 0：原生底板与体检（案：早期斑概览污染；449 三源比对）

- 分割底板必须是**本地、带 EPSG、真 1 m** 的栅格。网络 COG/瓦片层（如
  fengmiao_cog_URL，maxZoom 14）只作显示与灰度比对，**不作分割底板**。
- 源是无 CRS 的 COG/GeoTIFF 时：在转换器导出时写入 CRS
  （CGCS2000 3°带 39 → EPSG:4527；交付坐标带号 39＋东伪 500 km 已实证）。
  **勿依赖 `geolibre:assign_projection_raster`**：已知 bug 丢 geotransform
  （输出范围变全球 [-180,90,180,90]）；用过必查 extent。
- 大图幅整幅层默认读概览（≈4 m），显示与统计皆然。一切分析只在小裁剪块上做。
- **原生体检**：裁剪后立即分区统计，**面积(m²) ≈ 像元数（±10%）**才准继续
  （反例：331.75 m² 只摊 15 px＝概览污染，停止）。
- **换影像源先重标定**：源间灰度差（449 案实测 2.6）会改 threshold 手感；
  换源后重做体检并重标 threshold，再开切。

## 每个图斑（先选中该图斑；共用地图严格串行，一次一斑）

1. `geolibre:clip_raster_to_polygon` — 以**外扩 15 m 的缓冲面**裁剪（分割上下文；
   输出 ≈1 px/m，尺寸约 (长+30)×(宽+30) px）。
2. `geolibre:image_segmentation` — threshold=2.5，min_area=200。
   "太碎"→上调 threshold；"太粗"→下调（全批 19 斑标定经验）。
   **规则①（吞段诊断，案：128）**：建设类（TBLX=JZ）或小斑**只切出 1 段**、
   而影像内部明显是多个地块 → min_area 吞段：改 **min_area=30 重分**
   （128：228 px 整片 → 4 段 89/74/33/32 px，总和不变）。
   注意与真单面斑区分：444/446/447/448/453 本就是一块地，1 段是对的。
3. `geolibre:segments_merge_small_regions` — **面参数名是 segments**（规则④），
   segments=②，min_size=30，method=nearest。多面大斑（132 22 面级）同样适用。
4. `geolibre:boundary_clean` — method=majority，iterations=1。
5. **回裁图斑**（案：中期补入，早期斑无此步）：`geolibre:clip_raster_to_polygon`
   — input=④，polygons=图斑层本体（无缓冲）；外环回到图斑边界，产物 RECLIP 隐藏留档。
6. `geolibre:raster_to_vector_polygons` — input=⑤。
7. `geolibre:smooth_shared_edges` — algorithm=paek，tolerance=2～4（两档全批均验过，
   128 定稿 2），snap_tolerance=1，**smooth_boundary=false（强制，规则③）**。
   只平滑内部共用边：smooth_boundary=true 外环内缩 ~1.5%（128：222.12→218.83），
   0.5 m 补救对齐追不回（仍 −1.27%）→ 禁止，超差只能回栅格重分。
8. `geolibre:align_features` — target=图斑层（实测用其 EPSG:4326 重投影版），
   search_distance=1.5，**单程**。
   - 单面斑：1.5 单程即可；锯齿顶点不足时 densify 后再对齐可用
     （案：128 单面 33/33 顶点，−0.74%）。
   - **多面斑（规则②，案：128）**：只用 1.5 单程，禁 densify+二程（过拉 −3.77%）、
     禁 0.5 双程（拉成退化环，repair 只会丢面救回）。
     验后核对 align_max_disp / align_mean_disp。
9. `geolibre:repair_geometry` — check_only 验自相交与退化环；**其新增报告层
   验收后一并删除**。
10. `spatialJoin`（裸名）— target=⑨，join=图斑层，match="Largest overlap"。
11. `calculateGeometry`（裸名）— input=⑩，area_km2 报面积（测地口径）。
    多面斑逐面落 area_km2 并**保留逐面面积清单**（汇总表备料）。

## 验收一

- 像元体检（步骤 0）±10%。
- **面积（冻结口径）：Σ 各面测地面积 vs 图斑 Shape_Area 差 <1%**（无缝无叠时
  ＝并集口径；早期斑/单面斑按整体口径验收的成果不追溯重算）。
- 碎面：合并+回裁后 <30 m² 面数 = 0；若仍出现，**记录（面积/周长/位置）不删要素**。
- 无自相交、无退化环（步骤 9 报 0 问题）。
- smooth_shared_edges 报 "Collapsed ring count: 0"。

## 验收二

1. `erase`（input/erase）差集复核：红=多出、蓝=缺失；区分"成块溢漏"与
   "锯齿弦切带"（宽 0.2–0.5 m 属正常锯齿，真因多半在分割参数）。
2. 三线对比：绿=原始分割、青=真值图斑、黄=成果，同屏叠放。
3. 多面斑先定划分：`layer.symbology`（unique，VALUE）＋ `layer.transparency` 45
   分色快照，用户确认后再继续。

## 成果规范

- 转正层命名 `成果_<JCBH 后缀>`，转正即删旧成果层；RECLIP 隐藏留档。
- 成果样式：填充 `#00000000`，描边 `#FFD400` 宽 2。
- 收尾全图清点（案：357）：确认仅剩各成果层＋隐藏 RECLIP，中间层（诊断/实验/
  统计/repair 报告层）用 run_command `layer.remove` 清理；视图缩至本斑成果层。

## 批量与汇总

- 样板先行：首斑全流程+验收通过后才批量推广（案：088 起步经验）。
- 逐图斑重复 11 步；完成后 `merge` 成"地块分割成果"层。
- 用户只回地图快照不回字时，读图视为反馈/放行。
- 统计只准在裁剪小块上跑；整幅统计读概览、数值作废。
- 裁剪块颜色与整幅不一致：`raster.stretch` `like`=整幅层——显示归一化差异，
  非数据差异（案：449）。

## 禁止

- 不用 `geolibre:intersect`（毫米碎块）；回裁已在栅格步骤完成掩膜。
- 面积不用 `$area` 或 GeoLibre 面积工具，用 calculateGeometry 的 area_km2。
- 不在无 EPSG 的栅格上直接裁剪/统计；不用 extract_raster_values_at_points（EPSG:0）。
- 禁止 smooth_boundary=true；多面斑禁 densify+二程对齐、0.5 双程对齐。

## 工具 ID 备忘（全批实测，防踩空名）

- 存在：`geolibre:clip_raster_to_polygon` / `geolibre:image_segmentation` /
  `geolibre:segments_merge_small_regions`（segments） / `geolibre:boundary_clean` /
  `geolibre:raster_to_vector_polygons` / `geolibre:smooth_shared_edges`
  （algorithm paek 默认·bezier，tolerance 默认 1.0，smooth_boundary）/
  `geolibre:align_features` / `geolibre:repair_geometry` / `spatialJoin` /
  `calculateGeometry` / `erase`。
- 不存在：`geolibre:spatialJoin`、`geolibre:calculate_geometry`、裸的 repair_geometry（无前缀）。
- 命令层 `layer.symbology`（unique）、`layer.transparency` 可直接用。
