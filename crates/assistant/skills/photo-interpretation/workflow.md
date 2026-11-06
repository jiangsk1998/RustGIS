# 监测图斑地类判读与分割业务流程

本文把三个内置技能串成一条可执行的业务流程，说明「一斑一地类」与「一斑多地类」从影像分割到成果层的完整做法。

| 技能 | 文件 | 职责 |
|---|---|---|
| tb-segmentation-v3 | [tb-segmentation-v3/SKILL.md](../tb-segmentation-v3/SKILL.md) | 从 1 m 影像切分图斑内地块边界，不判地类 |
| photo-interpretation | [SKILL.md](SKILL.md) | 用外业举证照片判读地类，产出判定三字段 |
| result-projection | [result-projection/SKILL.md](../result-projection/SKILL.md) | 把判定结果投射到分割块，生成成果层 |

## 1. 总流程

```text
步骤 0  底板体检（本地 1 m 栅格、EPSG、面积≈像元数）
   │
步骤 1  图斑内分割 → 得到 N 个分割块
   │
步骤 2  照片判读 → 得到 M 个地类
   │
步骤 3  分流
   ├─ M = 1 ────────────→ 一斑一地类
   └─ M ≥ 2 ────────────→ 一斑多地类
   │
步骤 4  结果投射（SpatialJoin + 字段 + dissolve）
   │
步骤 5  验收与成果命名
```

分割只定边界、不判地类；判读只定地类、不改边界；两者在「结果投射」阶段合并。

## 2. 前置条件

- 图斑层：以 `JCBH` 标识，关键字段 `BSM`、`JCMJ`、`NM/NC`、`BGHDL`、`SFBG`、`LJBZ`。
- 照片点层 `FJ`：`FJ`(Image 照片)、`FJMC`、`DKBSM`、`PSSJ`、`PSJD`、`PSRY`，无 FID，用 `_fid`。
- 挂接关系：`FJ.DKBSM = 图斑.BSM`，按标识码关联，优于空间距离筛选。
- 分割底板：本地、带 EPSG、真 1 m 的栅格；网络 COG/瓦片层只作显示比对，不作底板。

## 3. 步骤 0：底板体检

1. 无 CRS 的 COG/GeoTIFF 在转换器导出时写入 CRS（CGCS2000 3°带 39 → EPSG:4527）。
   不要依赖 `geolibre:assign_projection_raster`：已知会丢 geotransform，用过必须检查 extent。
2. 大图幅整幅层默认读概览（约 4 m），一切分析只在小裁剪块上做。
3. 裁剪后立即分区统计：`面积(m²) ≈ 像元数（±10%）` 才可继续。
   反例：331.75 m² 只摊 15 个像元，说明读到概览，停止并换底板。
4. 换影像源后重新体检并重标 `threshold`（源间灰度差会改变分割手感）。

## 4. 步骤 1：图斑内分割

逐斑串行，一次一斑；先选中该图斑。

| 序 | 工具 | 关键参数 | 作用 |
|---|---|---|---|
| 1 | `geolibre:clip_raster_to_polygon` | 外扩 15 m 缓冲面 | 保留分割上下文，输出约 1 px/m |
| 2 | `geolibre:image_segmentation` | threshold=2.5，min_area=200 | 区域生长分割 |
| 3 | `geolibre:segments_merge_small_regions` | segments=②，min_size=30，method=nearest | 碎块并入最近邻 |
| 4 | `geolibre:boundary_clean` | method=majority，iterations=1 | 压掉孤立像元 |
| 5 | `geolibre:clip_raster_to_polygon` | input=④，图斑本体（无缓冲） | 回裁，外环回到图斑边界 |
| 6 | `geolibre:raster_to_vector_polygons` | input=⑤ | 矢量化 |
| 7 | `geolibre:smooth_shared_edges` | algorithm=paek，tolerance=2~4，snap=1，smooth_boundary=false | 只平滑内部共用边 |
| 8 | `geolibre:align_features` | target=图斑层，search_distance=1.5，单程 | 外环吸附图斑边界 |
| 9 | `geolibre:repair_geometry` | check_only | 验自相交/退化环，报告层验收后删除 |
| 10 | `spatialJoin` | join=图斑层，match=Largest overlap | 挂图斑属性 |
| 11 | `calculateGeometry` | area_km2 | 测地面积，保留逐面清单 |

### 分割结果决定分支起点

- 只切出 1 段，但影像内部明显是多个地块 → 多半是 `min_area` 吞段：
  改 `min_area=30` 重分后判断。
- 本就是一整块地（如 444/446/447/448/453）→ 1 段是正确的，不重分。
- 多段（全批 19 斑面数谱 1–22 面）→ 进入「一斑多地类」候选流程。

## 5. 步骤 2：照片判读

1. 读图斑属性线索（`NM/NC`、`BGHDL`、`SFBG`、`LJBZ`）。
   初始线索常被照片推翻，只作假设，不作结论。
2. `read_features(FJ, where "DKBSM = '<BSM>'")` 取该斑全部照片的 `_fid/FJMC/PSSJ/PSJD`。
3. 逐张 `read_image(layer='FJ', field='FJ', fid=…)`：记录地物、硬化/裸土/植被、设施类型、作物与残茬。
4. 对照知识库（本文同行录 SKILL.md 附录《国土调查地类知识库》）逐条比对二级类判读要点。
5. 硬约束：
   - 院内裸土不判 1201 空闲地；
   - 斑外棚舍不判 1202 设施农用地；
   - 耕作层已实质破坏不判耕地；
   - `BGHDL` 变更记载可作互证。
6. 元数据加固：`PSJD` 判视线指向（区分斑内/斑外），`PSSJ` 判现状时点与季节（跨多日补拍以最新为准）。
7. 产出三字段：**判定地类代码 / 判定地类名称 / 判定依据**。
   判定依据引用照片名（FJMC）＋照片所见＋知识库要点。

### 分流判定（M 为地类数）

| 现象 | 结论 |
|---|---|
| 照片集中围绕同一地物，未见独立地块 | **一斑一地类**，判读依据注明「一斑一地类」 |
| 照片显示多个独立地物，且分割块边界与之吻合 | **一斑多地类**，需逐块判定 |
| 照片里出现附属设施（棚舍、硬化场院等） | 仍按主类，不拆多地类 |
| 照片与部分分割块距离远（>30 m） | 不在判读阶段硬定，交给投射阶段标注举证距离 |

## 6. 步骤 3：结果投射

每斑标准链路：

1. `select.clear`：地理处理只作用选中要素且忽略 `layer` 参数，跑工具前必须清空选择。
2. `SpatialJoin`（`gp.run`）：目标=分割块层，连接=FJ 点层，**Closest，30 m**，保留最近照片距离。
   有块超过 30 m 未匹配 → 用 50 m 重跑另存（`成果_<斑号>_SpatialJoin2`）。
3. `edit.addField`（参数名是 `name`，不是 `field`；type=text）：
   `判定地类代码`、`判定地类名称`、`判定依据`、`拍摄时间`、`拍摄方位角`。
4. `edit.calculateField`：
   - 一斑一地类：直接写字符串字面量 `'<值>'`；
   - 一斑多地类：用 `CASE WHEN` 按 `VALUE`/位置逐块赋值；
   - DuckDB lambda 写 `lambda x, i:`（箭头函数报错）；
   - 拍摄时间：去重后单值直写，多值写「最早 ~ 最晚」；
   - 拍摄方位角：去重升序、`、` 连接、末尾加 `°`。
5. `dissolve`：按 `判定地类代码` 聚合
   `FIRST_判定地类名称`、`FIRST_判定依据`、`CONCATENATE_join_FJMC`（分隔「、」）、`count`。
6. 样式：填充 `#F7D774`、透明 25，标注 `FIRST_判定地类名称`。

### 一斑一地类

- 整斑一次判定，字段整列同值。
- dissolve 后 **成果层 1 个要素**；
- `CONCATENATE_join_FJMC` 汇总全部举证照片名。

### 一斑多地类

- 先确认划分：`layer.symbology`（unique，`VALUE`）＋ `layer.transparency 45` 出分色快照，
  用户确认后再继续。
- 逐块判定后按代码赋值，dissolve 后 **每个地类代码 1 个要素**，成果层要素数 = 地类数。
- 最近照片距离 >30 m 的块，在判定依据标「举证不足/待核」。

## 7. 步骤 4：验收

分割侧：

- 像元体检 ±10%；
- Σ 各面测地面积 vs 图斑 `Shape_Area` 差 <1%；
- 合并+回裁后 <30 m² 的面数为 0（若出现，记录面积/周长/位置，不删要素）；
- 无自相交、无退化环；
- `smooth_shared_edges` 报 `Collapsed ring count: 0`。

投射侧：

- `read_features` 核对 SpatialJoin 匹配数与成果层要素数；
- 一斑一地类成果层应为 1 个要素；一斑多地类应为地类代码数；
- 最近照片距离 >30 m 的块已标注「举证不足/待核」。

复核手段：

- `erase` 差集：红=多出、蓝=缺失；区分成块溢漏与 0.2–0.5 m 的锯齿弦切带；
- 三线对比：绿=原始分割、青=真值图斑、黄=成果。

## 8. 步骤 5：成果与命名

- 分割层转正命名 `成果_<JCBH 后缀>`，转正即删旧成果层，RECLIP 隐藏留档。
- 分割成果样式：填充 `#00000000`，描边 `#FFD400` 宽 2。
- 投射成果层命名 `<斑号>号图斑地类判定成果`。
- 批量：样板先行，首斑全流程验收通过后再推广；逐斑重复后 `merge` 成「地块分割成果」层。
- 收尾清理中间层（诊断/实验/统计/repair 报告层），视图缩至本斑成果层。

## 9. 字段与前缀约定

| 前缀 | 来源 | 含义 |
|---|---|---|
| `join_` | SpatialJoin 带入 | `join_FJMC`、`join_PSSJ`、`join_PSJD` 等 |
| `FIRST_` | dissolve 聚合 | 取组内首值（名称、依据） |
| `CONCATENATE_` | dissolve 聚合 | 组内拼接（照片名） |

## 10. 禁止事项

- 不用 `geolibre:intersect`（会产生毫米级碎块），回裁已在栅格步骤完成掩膜。
- 面积不用 `$area` 或 GeoLibre 面积工具，用 `calculateGeometry` 的 `area_km2`。
- 不在无 EPSG 的栅格上裁剪/统计；不用 `extract_raster_values_at_points`（输出 EPSG:0）。
- 禁止 `smooth_boundary=true`（外环会内缩约 1.5%）。
- 多面斑禁止 densify+二程对齐、0.5 m 双程对齐（会过拉或拉成退化环）。
- 整幅统计读的是概览，数值作废；统计只准在裁剪小块上跑。

## 11. 两种分支对照

| 维度 | 一斑一地类 | 一斑多地类 |
|---|---|---|
| 分流依据 | 照片只对应一个地类 | 照片/分割块显示多个独立地类 |
| 分割块处理 | 多块可同属一类 | 每块单独定类，先出分色快照确认 |
| 字段赋值 | 整列写同一字面量 | `CASE WHEN` 按 `VALUE`/位置逐块赋值 |
| dissolve 结果 | 1 个要素 | 每个地类代码 1 个要素 |
| 举证汇总 | 全部照片名拼进一个要素 | 按块挂最近照片，逐类汇总 |
| 验收要素数 | 1 | = 地类代码数 |
| 共同点 | 底板体检、分割 11 步、Closest 30 m 挂照片、面积 <1%、碎片为 0 | 同左 |
