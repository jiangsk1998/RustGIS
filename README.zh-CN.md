<h1 align="center">RustGIS</h1>

<p align="center">
  <b>纯 Rust 编写、快速、免费、开源的 GIS：制图、分析与共享地理空间数据。</b><br>
  桌面端支持 Windows、macOS、Linux，也能直接在浏览器里运行。数据留在你自己的机器上，
  除非你主动把它发给所选的 AI 模型；每个操作都是一条可以脚本化的命令。
</p>

<p align="center">
  <a href="README.md">English</a> | 简体中文 ·
  <a href="https://web.rustgis.app">在线使用</a> ·
  <a href="https://github.com/opengeos/RustGIS/releases/latest">下载桌面版</a> ·
  <a href="https://rustgis.app">文档</a>
</p>

## 为什么用 RustGIS

- **快而小。** 单个原生程序，GPU 绘制，不需要运行时，也不需要服务器。
- **到处都能跑。** 同一套程序覆盖 Windows、macOS、Linux，并编译成 WebAssembly 跑在
  [web.rustgis.app](https://web.rustgis.app)，无需安装。
- **本地、私密。** 文件在本机读写，不会自动上传。只有你主动使用时才会联网：底图瓦片、
  插件、GeoLibre 工具，以及你选择的 AI 模型。
- **一切皆命令。** 功能区上的每个按钮都是一条带 id 的命令，可用 `rustgis-cli` 脚本化、
  在笔记本和模型里链式调用，或让 AI 助手代你执行。
- **熟悉。** 按工作流程排布的 Ribbon（Home、Data、Style、Edit、Analyze、View、Publish）、
  图层与浏览器面板、属性表，以及所有修改都可撤销。
- **可扩展。** 1,000+ 个 GeoLibre / Whitebox 工具，以及沙箱化的 WebAssembly
  [插件](docs/plugins.md)，可新增命令、工具和 Ribbon 标签页。

## 主要特性

| 领域 | 亮点 |
|---|---|
| **数据** | GeoJSON、CSV/TSV、Excel、Shapefile（含压缩包与浏览器端）、GeoPackage、GeoParquet、FlatGeobuf、KML/KMZ、GPX、GML、OSM PBF、DXF、LiDAR（LAS/LAZ/COPC）、带地理标记的照片等；GDAL 支持的格式与 DuckDB 查询（需安装对应程序）；PostGIS、MySQL/MariaDB、SQL Server 实时连接（桌面端）；PMTiles/MBTiles 瓦片 |
| **栅格** | GeoTIFF / COG、ASCII grid、Zarr 2/3；任意 EPSG 重投影；拉伸、RGB、分类、唯一值、色带、山体阴影；DRA、波段组合、亮度/对比度/伽马；云优化 GeoTIFF 导出 |
| **地图** | 一个工程多张地图与版面；倾斜、旋转、地形、3D 地球；月球、火星等天体；Web Mercator 或任意投影；书签、范围历史、坐标导航；5 套离线底图 |
| **服务与瓦片** | WMS、WMTS、WFS、OGC API Features、ArcGIS REST；XYZ/TMS/WMS 栅格瓦片、Mapbox Vector Tiles、MapLibre 样式，带缓存与后台拉取 |
| **制图** | 单一符号、唯一值、分级色彩、比例符号；标注分类与 SQL 查询、沿曲线标注、字体、比例范围；线型、符号级别、掩膜、显示过滤、聚合分箱、2.5D 拉伸 |
| **编辑** | 新建、移动、旋转、缩放、合并、拆分、炸开要素；捕捉、模板、高程、导线、拓扑规则；域、子类型、属性规则、关系、归档；全程可撤销 |
| **分析** | 按属性与位置选择；Buffer、Near、Clip、Erase、Split、Intersect、Union、Dissolve、Spatial Join、Zonal Statistics；视域、通视、IDW、克里金、最短路径、适宜性建模；ModelBuilder 节点画布；1,000+ GeoLibre/Whitebox 工具 |
| **版面与分享** | 页面图框、图例、比例尺、网格、地图系列；报告与演示；PNG（任意 DPI）、PDF、Web 地图页、OGC API Features、glTF 导出 |
| **AI 助手** | 用自然语言提问或指挥地图；支持 Anthropic、OpenAI（API Key 或登录 ChatGPT 套餐）、Gemini、OpenRouter、Ollama 或任意 OpenAI 兼容服务；另提供 `rustgis-cli ask` 与 [MCP 服务](docs/mcp.md) |
| **插件** | 沙箱化 WebAssembly [插件](docs/plugins.md) 可新增命令、工具和 Ribbon 标签页，从 [plugins.rustgis.app](https://plugins.rustgis.app) 安装 |
| **界面** | 图层/浏览器/数据源面板、命令搜索（Ctrl+K）、快捷键、深色模式、可定制 Ribbon、并排联动地图、19 种语言 |

用户手册见 <https://rustgis.app/user-guide/>，规划见 [ROADMAP.md](ROADMAP.md)。

## 获取 RustGIS

- **网页版**：打开 **[web.rustgis.app](https://web.rustgis.app)**，无需安装。
- **桌面版**：从 [downloads.rustgis.app](https://downloads.rustgis.app) 或
  [最新 Release](https://github.com/opengeos/RustGIS/releases/latest) 下载。安装细节见
  [Installation](docs/installation.md)。
- **包管理器**：`winget install OpenGeos.RustGIS`、
  `brew install --cask opengeos/rustgis/rustgis`、`yay -S rustgis-bin`（AUR）、
  `sudo dnf copr enable giswqs/rustgis && sudo dnf install rustgis`（Fedora）。
- **从源码构建**：`cargo run --release -p rustgis`（需要 Rust 1.90 或更新版本）。

## 快速开始（从源码）

### 1. 准备环境

- **Rust 1.90+**（稳定版即可，nightly 也能用）。没装的话：

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

- **C 编译器**（编译内置 SQLite 与 Zstandard 解码器）：Linux 上是 `build-essential` 或
  `gcc`/`clang`。
- **curl**：构建时用来下载 Natural Earth 示例数据。
- 桌面版需要可用的图形环境与显卡驱动（Linux 上默认 Vulkan，可回退 OpenGL）。

Ubuntu/Debian 上一次装齐：

```sh
sudo apt install build-essential pkg-config libssl-dev curl
```

### 2. 编译并启动桌面版

```sh
git clone https://github.com/opengeos/RustGIS.git
cd RustGIS

cargo run --release -p rustgis                 # 打开 World Atlas 示例工程
cargo run --release -p rustgis -- --sample     # 总是打开示例工程
cargo run --release -p rustgis -- data.geojson # 打开数据文件
cargo run --release -p rustgis -- atlas.rgis   # 打开已保存的工程
```

首次编译会拉取依赖并下载示例数据，耗时较长；之后启动很快。若显卡驱动导致窗口打不开，
可换渲染后端：

```sh
cargo run --release -p rustgis -- --backend gl       # OpenGL
cargo run --release -p rustgis -- --backend vulkan   # Vulkan（Linux 默认）
```

也可以直接运行编译好的二进制：

```sh
cargo build --release -p rustgis
./target/release/rustgis
```

离线构建：先下载 `countries.geojson`、`countries_chn.geojson`、`cities.geojson`、
`rivers.geojson`、`lakes.geojson`（<https://assets.rustgis.app/data/>），再指定目录：

```sh
RUSTGIS_DATA_DIR=/path/to/data cargo build --release
```

### 3. 启动命令行工具

命令行里每个 Ribbon 动作都是一条带 id 的命令：

```sh
cargo build --release -p rustgis-cli
./target/release/rustgis-cli commands          # 列出所有命令及其参数
./target/release/rustgis-cli render countries.geojson map.png --basemap ocean
./target/release/rustgis-cli run --sample \
  --cmd 'gp.run={"tool":"buffer","params":{"input":"Cities","distance":100}}' \
  --cmd 'share.exportMap={"path":"out.png","dpi":192}' --save atlas.rgis
./target/release/rustgis-cli ask --sample "人口最多的 5 个国家是哪些？"
```

详见 [Command Line](docs/cli.md)。

### 4. 启动网页版（WebAssembly）

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk
cd apps/rustgis-web
trunk serve            # http://127.0.0.1:8772
```

浏览器构建需要 clang（它把 Zstandard 解码器编译成 WebAssembly）。也可以直接用 Docker：

```sh
docker run --rm -p 8080:8080 ghcr.io/opengeos/rustgis   # http://localhost:8080
```

详见 [Installation](docs/installation.md#web-app)。

### 5. Python 包（可选）

```sh
pip install rustgis
```

```python
import rustgis
s = rustgis.Session(sample=True)
s.run("gp.run", tool="buffer", params={"input": "Cities", "distance": 100})
s.to_png("map.png")
```

详见 [Python](docs/python.md)。

## 上手五分钟

1. **浏览**：拖动平移、滚轮缩放、点击国家查看属性，按 `Home` 回到全图。
2. **样式**：在图层列表点 *Countries*，打开 **Style › Symbology**，选 *Graduated Colors*，
   字段用 `pop_est`。
3. **选择**：**Home › Select By Attributes**，输入 `continent = 'Asia'`，按 *Apply*。
4. **分析**：**Analyze › Buffer**，输入 *Cities*，距离 `300`，单位 *Kilometers*，勾选 *Dissolve*。
5. **分享**：**Publish › Export Map** 导出 PNG；**Project › Save As** 保存 `.rgis` 工程。

完整流程见 [Getting Started](docs/getting-started.md)。

## 架构

分层 crate，每层只依赖其下层：

| 层 | Crate | 职责 |
|---|---|---|
| L0 | `rustgis-geom` | 坐标、范围、Web Mercator、测地线距离/面积、比例尺 |
| L1 | `rustgis-data` | 要素与类型化属性；各类矢量格式、数据库提供者、SQL 过滤、栅格与坐标系 |
| L2 | `rustgis-map`、`rustgis-analysis` | 图层、符号化、标注、相机、底图；地理处理工具 |
| L2 | `rustgis-tiles` | 瓦片：XYZ/TMS/WMS、MVT、PMTiles/MBTiles、MapLibre 样式、瓦片缓存 |
| L3 | `rustgis-render` | tiny-skia 渲染器、标注布局、PNG 导出、字体 |
| L3 | `rustgis-plugins` | 沙箱化 WebAssembly 插件与插件仓库 |
| L3 | `rustgis-geolibre` | GeoLibre 工具套件 |
| L4 | `rustgis-engine` | Session、撤销、命令注册表、示例工程 |
| L5 | `rustgis-assistant` | AI 助手：通过命令行动的对话代理 |
| L6 | `rustgis-ui-egui` | Ribbon 前端（可替换） |
| apps | `rustgis`、`rustgis-cli`、`rustgis-web`、`rustgis-python` | 桌面端、命令行、浏览器（WebAssembly）、Python 包 |

`cargo xtask ci` 会运行格式化、clippy、测试和分层检查。贡献者与 agent 说明见
[AGENTS.md](AGENTS.md)。

## 常见问题

- **首次编译很慢 / 停在下载数据？** 构建脚本会在编译时下载 Natural Earth 示例数据，
  网络不通时用 `RUSTGIS_DATA_DIR` 指向本地数据目录。
- **窗口打不开、直接退出？** 多半是显卡驱动问题，试 `--backend gl`，或升级驱动。
- **`cargo xtask ci` 报分层错误？** 低于 L6 的 crate 不能依赖 egui/eframe/rfd。
- **浏览器构建失败（Zstandard/`cc`）？** 需要 clang；macOS 上 Apple 的 clang 不行，
  需 `brew install llvm` 并把其 `bin` 放到 `PATH` 前面。

## 许可证与致谢

基于 [Apache License 2.0](LICENSE) 授权。示例数据来自
[Natural Earth](https://www.naturalearthdata.com/)（公有领域）。第三方组件与素材来源见
[ATTRIBUTION.md](ATTRIBUTION.md) 与 [Acknowledgements](docs/acknowledgements.md)。

## 引用

> Wu, Q., Rincón Albarracín, J. G., Filho, E., Hofbauer, D., Fitzgerald, M., & Strebel, D. (2026).
> RustGIS: A fast, free and open-source GIS in pure Rust for mapping, analyzing and sharing
> geospatial data. Zenodo. <https://doi.org/10.5281/zenodo.23633281>
