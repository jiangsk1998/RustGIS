---
name: tb-segmentation-v4
description: 监测图斑内地类边界分割（v4：拓扑构建 + 质检定稿）——从 1 m 影像切出地块边界，再做几何规整、无重叠/无缝隙/共节点/无碎片拓扑构建，以及面积闭合、双向差集、照片交叉验证的量化质检。
---

# 监测图斑地类边界分割（v4：拓扑构建 + 质检定稿）

v3 的影像原则全部继承：**只从影像纹理/颜色切边界、不判地类、不降分辨率、一次一斑串行、
大图幅禁全幅分析、换源先重标定**。v4 在其后加两段：**拓扑关系构建**（第 3 节）与
**量化质检定稿**（第 4 节），并固化已实证的坑位解法。

外环＝图斑原始边界；内部界线为平滑曲线，且邻面共节点共边。图斑层以 JCBH 标识。
规则括注来源：v2 批量经验（088→453 共 17 斑）＋ 128 收官案 ＋ v4 拓扑/质检实证。

## 步骤 0：前置体检（每次必做）

- 图斑：共享图斑层按 `JCBH` 选中 **1 斑**，记下 `Shape_Area`（m²，验收基准）。
- 底板：**本地、带 EPSG、真 1 m** 的栅格；网络 COG/瓦片层只作显示与灰度比对，
  **不作分割底板**。
- **无 CRS 标签的 TIFF**（GeoAscii 有投影名/带号，TIFF 头里没有）：
  - **勿用 `geolibre:assign_projection_raster`**：已复现丢 geotransform
    （extent 变 `[-180,90,180,90]`），用过必 undo。
  - 先 `geolibre:print_geotiff_tags` 读 tiepoint/scale/投影名：带号前缀
    （x≈39,5xx,xxx）→ CGCS2000 3°带 zone 39 = **EPSG:4527**。
  - 用 `geolibre:warp_raster` 四角 GCP 恒等仿射逐波段重写：
    poly1、nearest、cell_size=1、epsg=4527，
    `gcps="0,0,x0+0.5sx,y0-0.5sy; W-1,0,…; W-1,H-1,…; 0,H-1,…"`。
    RMS 应≈0、网格尺寸不变、extent 与原显示范围亚米内一致。
    三波段**跑三次**（band=1/2/3；**漏传 band 会得到三份红波段**）。
- 裁剪（外扩 15 m → `geolibre:clip_raster_to_polygon`）后核对 size：
  px 尺寸 ≈ 米数（1 px=1 m），超过 2 倍即概览污染（≈4 m），**禁做分析**。
- **原生体检**：裁剪块像元数 ≈ 缓冲面积 ±10% 才准继续
  （反例：331.75 m² 只摊 15 px＝概览污染，停止）。
- **换影像源先重标定**：源间灰度差（449 案实测 2.6）会改 threshold 手感；
  换源后重做体检并重标 threshold，再开切。

## 步骤 1：分割链（v3 同款）

先选中该图斑；共用地图**严格串行，一次一斑**。

1. `geolibre:clip_raster_to_polygon` — 以**外扩 15 m 缓冲面**裁剪（分割上下文；
   输出 ≈1 px/m，尺寸约 (长+30)×(宽+30) px）。
2. `geolibre:image_segmentation` — threshold=2.5，min_area=200。
   "太碎"→上调 threshold；"太粗"→下调。
   **规则①（吞段诊断，案：128）**：建设类（TBLX=JZ）或小斑**只切出 1 段**、
   而影像内部明显是多个地块 → min_area 吞段：改 **min_area=30 重分**
   （128：228 px 整片 → 4 段 89/74/33/32 px，总和不变）。
   注意与真单面斑区分：444/446/447/448/453 本就是一块地，1 段是对的。
   - **备选路径（数组参数绑不上时）**：`geolibre:composite_bands` 的 `inputs` 为数组参数，
     当前绑定失败；改走 `geolibre:create_colour_composite` 先打包 RGB，
     再把多波段栅格作**单一 input** 传入。
3. `geolibre:segments_merge_small_regions` — **面参数名是 segments**（规则④），
   segments=②，min_size=30，method=nearest。多面大斑（132 22 面级）同样适用。
4. `geolibre:boundary_clean` — method=majority，iterations=1。
5. **回裁图斑本体**：`geolibre:clip_raster_to_polygon` — input=④，
   polygons=图斑层本体（无缓冲）；外环回到图斑边界，产物 RECLIP 隐藏留档。
6. `geolibre:raster_to_vector_polygons` — input=⑤。
7. `geolibre:smooth_shared_edges` — algorithm=paek，tolerance=2～4（128 定稿 2），
   snap_tolerance=1，**smooth_boundary=false（强制，规则③）**。
   只平滑内部共用边：smooth_boundary=true 外环内缩 ~1.5%（128：222.12→218.83），
   0.5 m 补救对齐追不回（仍 −1.27%）→ 禁止，超差只能回栅格重分。
   报 **Collapsed ring count: 0** 才继续。
8. `geolibre:align_features` — target=图斑层（实测用其 EPSG:4326 重投影版），
   search_distance=1.5，**单程**。
   - 单面斑：1.5 单程即可；锯齿顶点不足时 densify 后再对齐可用
     （案：128 单面 33/33 顶点，−0.74%）。
   - **多面斑（规则②，案：128）**：只用 1.5 单程，禁 densify+二程（过拉 −3.77%）、
     禁 0.5 双程（拉成退化环，repair 只会丢面救回）。
   - 记录 align_mean_disp / align_max_disp；**max 应 ≤1.5**。
9. `geolibre:repair_geometry` — check_only 验自相交与退化环：
   有自相交才 repair（自并）；无问题则不动几何（见规则⑤，repair 会破坏共节点）。
   其新增报告层验收后一并删除。
10. `spatialJoin`（裸名）— target=⑨（步骤 9 的产物），join=图斑层，
    match="Largest overlap"。
11. `calculateGeometry`（裸名）— input=⑩，area_km2 报面积（测地口径）。
    多面斑逐面落 area_km2 并**保留逐面面积清单**（汇总表备料）。

## 步骤 2：几何规整（顺序不可乱）

`geolibre:smooth_shared_edges`（步骤 1.7）→ `geolibre:align_features`（1.8）→
`geolibre:repair_geometry`（check_only）（1.9）。顺序本身就是正确性：
先平滑再对齐，对齐后只验不并；修复自并排在改几何之后、拓扑之前（第 3 节会再验一次）。

## 步骤 3：拓扑构建（v4 新增，顺序即正确性）

在几何规整**全部完成之后**做，施工顺序固定：

1. **自相交**：`geolibre:repair_geometry`（check_only）→ 有问题才 repair（仅此时）。
2. **无重叠**：`geolibre:topology_rule_validate`（默认 6 规则）→
   若报 polygon_must_not_overlap → `geolibre:remove_overlap_multiple`
   （method=center_line）。
   注意：repair（自并）可能新造重叠，报错数会多于目测，按工具报的来。
3. **无碎片**：`geolibre:eliminate_polygons`（max_area=30，method=longest_border），
   返回的 `Unmerged` 应=0；仍 >0 的碎块记录（面积/周长/位置）不硬删。
4. **共节点共边**：`geolibre:integrate`（tolerance=0.05，snap_to_edges=true）——
   **必须放在所有改几何步骤之后**。
   **规则⑤（已实证坑）**：repair（自并）会重建环、破坏 integrate 的共节点
   （出现重叠＋邻接丢失），故 integrate 必须最后。
5. **复检闭环**（三条全过才收工，否则按 1→4 再走一轮，**至多 2 轮**）：
   a. `geolibre:topology_rule_validate` → 0 行；
   b. `geolibre:repair_geometry`（check_only）→ 0 问题；
   c. `geolibre:polygon_neighbors` → 每面至少 1 条 `length>0` 的共用边、覆盖连通、
   无 0 长点接触残留。
   - **规则⑥**：integrate 微移偶发 2 个自相交时：repair → 再 integrate → 再复检。
   - 复检不过不得进入第 4 节；先回到本条第 1 步。

## 步骤 4：量化质检（全过为 A 级）

1. **面积闭合**：`calculateGeometry` 后 Σ各面测地面积 / 图斑 `Shape_Area` ∈ **±1%**
   （无缝无叠时＝并集口径）。
2. **差集复核**：`erase` 双向：
   - 面 − 图斑 = 溢出（红）；
   - 图斑 − 面 = 缺失（蓝）。
     仅允许 **0.2–0.5 m 锯齿弦切带**，不得有成块溢漏。
     复核时做**三线对比**：绿=原始分割、青=真值图斑、黄=成果，同屏叠放。
     **规则⑦（工具 bug）**：erase 细带上的测地面积可能算出天文数字，
     **看几何形状不论数值**；成块溢漏的真因多半在分割参数，回第 1 节重切。
3. **碎片**：面最小面积 ≥30 m²，`<30 m²` 计数=0
   （与步骤 3.3 的 Unmerged 对账）。
4. **照片交叉验证**：
   - FJ 按 bbox 选斑内照片 → `spatialJoin`（target=面，join=FJ，match=Intersects，
     one-to-one，输出 count + join 的 FJMC）。
   - 抽 **2 张骑线照片** `read_image` 目视核对：影像界线＝实物界线。
5. **多面斑先定划分**（用户确认后再继续）：
   `layer.symbology`（unique，VALUE）＋ `layer.transparency` 45 出分色快照。
6. QC 表（Topology Rule Validate / Polygon Neighbors）**留档**，不删。

## 步骤 5：成果与收尾

- 转正：最终层 `rename` 为 `成果_<JCBH 后缀>`；**转正即删旧成果层**。
- 成果样式：填充 `#00000000`（`layer.color`），描边 `#FFD400`（`layer.outline`）宽 2。
- 只留三样：**成果层、RECLIP（回裁底板，隐藏）、图斑层**。
  warp 全镇波段、裁剪块、分割链中间层、诊断/实验/统计/repair 报告层
  全部用 `layer.remove` 删除；视图缩至本斑成果层。
- 收尾全图清点（案：357）：确认没有多余中间层遗留。

## 批量与汇总

- 样板先行：首斑全流程＋第 3、4 节全过才批量推广（案：088 起步经验）。
- 逐图斑重复上述步骤；完成后 `merge` 成"地块分割成果"层。
- 用户只回地图快照不回字时，读图视为反馈/放行。
- 统计只准在裁剪小块上跑；整幅统计读概览、数值作废。
- 裁剪块颜色与整幅不一致：`raster.stretch` 的 `like`=整幅层——显示归一化差异，
  非数据差异（案：449）。

## 禁止

- 不判地类；不降分辨率；不整幅分析；不并斑并行。
- 不用 `geolibre:intersect`（毫米碎块）；回裁已在栅格步骤完成掩膜。
- 面积不用 `$area` 或 GeoLibre 面积工具，用 `calculateGeometry` 的 area_km2。
- 不在无 EPSG 的栅格上直接裁剪/统计；不用 extract_raster_values_at_points（EPSG:0）。
- 禁止 `smooth_boundary=true`；多面斑禁 densify+二程对齐、0.5 双程对齐。
- 禁止在 integrate 之后再 repair/align/改几何（会把共节点重新打散）。
- 禁止用 `geolibre:assign_projection_raster` 补 CRS（丢 geotransform）。
- 禁止删 QC 表；禁止把 `erase` 细带的面积数值当验收依据。

## 工具 ID 备忘（实测，防踩空名）

- 分割/几何：`geolibre:clip_raster_to_polygon` / `geolibre:image_segmentation` /
  `geolibre:segments_merge_small_regions`（segments） / `geolibre:boundary_clean` /
  `geolibre:raster_to_vector_polygons` / `geolibre:smooth_shared_edges`
  （algorithm paek 默认·bezier，tolerance 默认 1.0，smooth_boundary） /
  `geolibre:align_features` / `geolibre:repair_geometry` / `spatialJoin` /
  `calculateGeometry` / `erase`。
- 栅格头与重写：`geolibre:print_geotiff_tags` / `geolibre:warp_raster` /
  `geolibre:create_colour_composite` / `geolibre:composite_bands`（inputs 数组参数当前
  绑不上，走合成包装）。
- 拓扑/质检：`geolibre:topology_rule_validate` / `geolibre:remove_overlap_multiple` /
  `geolibre:eliminate_polygons` / `geolibre:integrate` / `geolibre:polygon_neighbors`。
  拓扑类工具的 id 以工具箱实际条目为准（显示名 Topology Rule Validate /
  Remove Overlap Multiple / Eliminate Polygon / Integrate / Polygon Neighbors）；
  报"工具不存在"时用工具搜索按显示名确认，不要改用同名近似工具。
- 不存在：`geolibre:spatialJoin`、`geolibre:calculate_geometry`、裸的
  repair_geometry（无 geolibre: 前缀）。
- 命令层 `layer.symbology`（unique）、`layer.transparency`、`layer.remove`、
  `raster.stretch` 可直接用。

## 环境坑备忘

- **数组型工具参数**（`inputs` 等）当前绑定失败：一律走"单参数 / 文本逗号串 /
  打包合成"绕行。
- **rule_set 类 Layer 参数**会被当图层名解析：`geolibre:topology_rule_validate` 用**默认全规则**，
  不传 rule_set。
- `geolibre:assign_projection_raster` 丢 geotransform（已复现，见步骤 0）。
- `geolibre:warp_raster` 漏传 `band` → 三份红波段（逐波段跑三次）。
- 大图幅整幅层默认读概览（≈4 m），显示与统计皆然；一切分析只在小裁剪块上做。
