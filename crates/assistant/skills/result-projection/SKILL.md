---
name: result-projection
description: 把逐斑地类判定结果投射到图斑内部分割块并生成"一斑一成果"图层：SpatialJoin Closest 挂照片、建判定与举证字段（含拍摄时间/方位角/最近照片距离）、按地类 dissolve、配样式标注。用于"结果投射""生成成果层""判定结果落到分割块""spatial join 投射"。
---

# 判读结果投射到分割块（一斑一成果）

## 前置
- 已完成 photo-interpretation（有判定三字段文本）；有分割块层
  （tb-segmentation-v3 产物，字段 VALUE、area_km2 等）与 FJ 照片点层。

## 标准链路（每斑）
1. **select.clear** —— 本项目 geoprocessing 只作用选中要素且忽略 layer 参数，
   跑任何工具前必须清空选择。
2. SpatialJoin（gp.run）：目标=分割块层，连接=FJ 照片点层，**Closest，30 m**，
   保留距离字段（最近照片距离）。有块超 30 m 未匹配 → 用 50 m 重跑另存
   （命名 成果_<斑号>_SpatialJoin2），成果层从新版 dissolve。
3. edit.addField（**参数名是 `name` 不是 `field`**，type 用 text）：
   判定地类代码、判定地类名称、判定依据、拍摄时间、拍摄方位角。
4. edit.calculateField：单值斑直接写字符串字面量 `'<值>'`；分块多地类用
   CASE WHEN 按 VALUE/位置赋值。DuckDB lambda 写 `lambda x, i:`（箭头函数报错）。
   - 拍摄时间：join_PSSJ 去重，单值直写，多值写"最早 ~ 最晚"；
   - 拍摄方位角：join_PSJD 去重升序、"、"连接、末尾加 °。
5. dissolve 按 判定地类代码：聚合 FIRST_判定地类名称、FIRST_判定依据、
   CONCATENATE_join_FJMC（分隔"、"）、count。一斑一地类→成果层 1 要素；
   多地类→每代码一要素。
6. 样式：layer.color #F7D774、透明 25；标注 FIRST_判定地类名称。
7. 验收：read_features 核对 SpatialJoin 层匹配数与成果层要素数；最近照片距离
   >30 m 的块在判定依据标"举证不足/待核"。

## 字段前缀
- `join_`＝SpatialJoin 带入（join_FJMC、join_PSSJ、join_PSJD…）
- `FIRST_`/`CONCATENATE_`＝dissolve 聚合产物
- 命名约定：分割块层 成果_<斑号>_SpatialJoin，成果层 <斑号>号图斑地类判定成果。
