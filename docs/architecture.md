# 架构

目标：把“PDF 能力”“阅读器状态”“GPUI 界面”分干净，后面加功能（表单、批注、多页滚动、AI 侧栏……）时不需要改一团乱麻。

## 分层

```
┌─────────────────────────────────────────────┐
│ crates/gpui-pdf（二进制）                     │
│  app.rs      控制器：引擎事件、键盘/鼠标、生命周期 │
│  home.rs     首页状态：最近阅读、搜索、侧栏       │
│  settings.rs 用户设置：主题、词典路径             │
│  dictionary.rs 词典面板状态：当前词/加载/结果     │
│  progress.rs 阅读进度模型：页码/滚动比例/缩放     │
│  viewer.rs   阅读器状态：文档/页/缩放/滚动       │
│  layout.rs   适宽/适页缩放、窗口尺寸（纯计算）    │
│  selection.rs / search.rs / highlights.rs     │
│  geometry.rs 纯几何工具                        │
│  ui/*        纯呈现（impl AppState）           │
│  ui/text_input.rs 单行输入框实体（IME）        │
└───────────────┬─────────────────────────────┘
                │ Command / Event（mpsc channel）
┌───────────────▼─────────────────────────────┐
│ crates/pdf-core（库，无 UI 依赖）              │
│  engine.rs  PDFium worker：打开/渲染/提取       │
│  model.rs   RectPt / CharBox / Link / Outline │
└─────────────────────────────────────────────┘
                │ Command / Event（mpsc channel）
┌───────────────▼─────────────────────────────┐
│ crates/dict-core（库，无 UI 依赖）             │
│  engine.rs  MDX worker：加载/查词/@@@LINK 跳转  │
│  css.rs     CSS 解析/层叠/计算样式              │
│  html.rs    释义 HTML → Block/Span/Table       │
│  fonts.rs   WOFF 解包 + 字体覆盖范围           │
│  model.rs   DictEntry / Block / Span           │
└─────────────────────────────────────────────┘
```

## 各模块职责

| 模块 | 负责 | 不负责 |
|---|---|---|
| `pdf-core::engine` | PDFium 生命周期、线程、渲染成 BGRA、文本/链接/目录提取、全文搜索计数 | 布局、坐标换算、交互 |
| `pdf-core::model` | UI 无关的数据类型 | 任何 GPUI 类型 |
| `dict-core::engine` | MDX 线程、加载词典、查询与重定向、读取同名/拆分 MDD、外置 CSS 和字体 | 渲染、设置读写 |
| `dict-core::css` | 用 `lightningcss` 解析样式表，用 `kuchiki`/Servo selectors 匹配选择器，做层叠与继承 | 元素布局、绘制 |
| `dict-core::html` | 用 `kuchiki` 解析 HTML；计算样式、`::before`/`::after` 内容、表格、图片和按字符选字体 | 完整浏览器布局（float/flex/columns） |
| `dict-core::fonts` | 解包 WOFF、解析 cmap 覆盖范围、给 UI 提供字体字节 | 字体栅格化 |
| `dict-core::model` | 词典条目、段落、行内样式、表格、图片 | 任何 GPUI 类型 |
| `app::AppState` | 引擎事件 → Viewer 更新；键盘/鼠标分派；打开文件 | 具体控件绘制 |
| `settings::SettingsStore` | 主题与词典路径的读写 | 词典解析 |
| `dictionary::DictionaryState` | 词典窗口的开关、当前词、加载/查询状态 | 词典解析、磁盘 |
| `dictionary::DictionaryView` | 推送给独立词典窗口的最小快照 | 查询逻辑 |
| `home::HomeState` | 最近阅读持久化（含阅读进度）、首页搜索过滤、侧栏选中项 | GPUI 控件 |
| `progress::ReadingProgress` | 页码/页内滚动比例/缩放/适应模式的数据模型与百分比换算 | 磁盘读写、视口换算 |
| `layout` | 页边距、适宽/适页缩放、按纸张比例算窗口尺寸 | 绘制、窗口 API |
| `viewer::Viewer` | 当前文档、页、缩放、滚动、页面缓存，恢复阅读位置，聚合 selection/search/highlights | GPUI 事件 |
| `selection` | 锚点/焦点、矩形合并、复制文本、双击选词 | 绘制 |
| `search` | 本页匹配区间、全文各页命中数、跨页跳转的目标（页 + 页内序号） | 键盘输入、绘制 |
| `highlights` | 高亮模型（稳定 `id` + 选中文字）、sidecar JSON 读写、删除 | 屏幕坐标 |
| `geometry` | 矩形合并等纯函数 | — |
| `ui::text_input` | 单行输入框：IME 组合、选区、光标绘制、编辑 action | 业务语义（搜什么、查什么） |
| `ui::annotations` | 批注侧栏：高亮列表、跳页、删除 | 持久化（走 `AppState`） |
| `ui::home` | 首页布局：侧栏、顶栏搜索、拖拽区、最近阅读卡片 | 磁盘写入 |
| `ui::chrome` | 顶部/底部浮动胶囊、窗口按钮、拖拽条 | 状态变更 |
| `ui::page` | 滚动容器、页面位图、覆盖层、链接热区 | 命中测试 |
| `ui::outline` | 浮动目录面板 | 书签解析 |
| `ui::dictionary` | 独立词典窗口：主窗口查词后自动弹出/更新 | 词典查询、窗口定位 |
| `ui::settings` | 设置面板：主题、词典路径 | 设置持久化 |
| `ui::empty` / `ui::toast` | 空状态、瞬时提示 | — |

## 数据流

1. UI 触发 `Command::Open(path)` / `Command::Render { page, scale, request }`。
2. `pdf-core` worker 在独立线程处理，回传 `Event::Opened` / `Event::Rendered` / `Event::Error`。
3. `AppState::handle_event` 更新 `Viewer`；打开成功时写入 `HomeState` 的最近阅读并恢复上次的页码/缩放（或适宽/适页模式）/页内滚动，
   `cx.notify()` 触发重绘。打开、切页、缩放、回到首页和退出时把阅读进度落盘；页内滚动由每秒一次的采样兜住。
   打开文档后 `Render` 依据当前显示器工作区调整窗口尺寸；之后每次 `viewport` 变化都按当前适应模式重算缩放。
4. `Render` 里调用 `ui::*` 的方法把 `Viewer` / `HomeState` 映射成 element 树。
5. 首页状态下 `ui::home` 读取 `HomeState`；有文档时改画 `ui::page`，顶部胶囊的「首页」清空 `Viewer` 回到首页。
6. 双击 PDF 文字 → `Selection::select_word` 选中英文单词或中文单字 → `AppState::lookup_word` 发送 `dict_core::Command::Lookup`；
   MDX worker 回传 `Found/NotFound/Error`，`DictionaryState` 更新后同步到独立词典窗口（放在主窗口右侧，不遮挡页面）。
   设置面板选择 `.mdx` 时写入 `settings.json` 并发送 `Command::Load`。
7. 加载词典时 worker 会同时打开同名 `.mdd` 和拆分的 `.1.mdd` / `.2.mdd`，读取 MDX 头部描述里引用的外置 CSS，
   把 `@font-face` 的 WOFF 解包成 sfnt 后通过 `Event::Loaded` 交给 UI 注册到 GPUI 文本系统。
   每条释义的 HTML/CSS 都在 worker 里转成 `Block`，UI 只负责把 run、表格和图片画出来。
8. 文本输入走平台输入通道：`ui::text_input` 在 paint 时把 `ElementInputHandler` 挂到窗口，
   IME/键盘的字符通过 `replace_text_in_range` / `replace_and_mark_text_in_range` 落到
   `TextField`；输入框把 `Changed` / `Submitted` / `Cancelled` 作为事件抛给外层实体。
   `AppState` 订阅后同步 `viewer.search.query`，`DictionaryWindow` 订阅后把词发回主窗口查词。
9. 全文搜索：输入变化 → `Command::Search` → worker 逐页提取文本（按页缓存）并统计命中 →
   `Event::SearchResults` 回每页命中数。`Enter` 用全局序号换算出「页 + 页内序号」，
   跨页时先 `set_page`，等 `Event::Rendered` 带回字符数据后再定位页内第 N 个匹配。

约束：

- **pdf-core / dict-core 不依赖 gpui**，可以直接写单元测试，也可以以后换渲染后端。
- **UI 不直接调 PDFium**，只读 Viewer。
- **坐标换算只在一处**：`AppState::page_origin` / `to_pdf`，PDF pt（左下原点）↔ 屏幕 px（左上原点）。
- **PDFium 非线程安全**：所有 PDFium 调用都在 worker 线程；UI 只拿结果。
- **HTML/CSS 语法解析不自研**：`kuchiki`（html5ever + Servo selectors）负责 HTML 与选择器，`lightningcss` 负责 CSS；`dict-core` 只做层叠、继承和 GPUI 模型转换。

## 加功能的落点

| 想加什么 | 改哪里 |
|---|---|
| 首页新导航页 / 最近、收藏、设置 | `ui/home.rs` + `home.rs`（侧栏状态） |
| 真实 PDF 首页缩略图 | `pdf-core` 增加缩略图命令 + `ui/home.rs` 缓存 |
| 新的浮动按钮/菜单 | `ui/chrome.rs` + `ui/mod.rs` |
| 新的快捷键 | `app.rs::on_key_down` |
| 新的阅读状态（如连续滚动位置、阅读进度） | `progress.rs`（模型）+ `viewer.rs`（恢复）+ `app.rs`（采样与落盘） |
| 新的文本交互（选区扩展、按词移动） | `selection.rs` + `app.rs` 的鼠标处理 |
| 词典 / 查词 / 多词典 | `dict-core`（解析与查询）+ `dictionary.rs` + `ui::dictionary` + `settings.rs` |
| 新的搜索能力（正则、高亮全部、结果列表） | `search.rs` + `pdf-core`（计数逻辑在 worker） |
| 新的输入框（跳页、重命名批注） | 复用 `ui::text_input`，外层订阅 `TextInputEvent` |
| 新批注类型（下划线、便签） | `highlights.rs` 泛化为 `annotations.rs` + `ui::annotations` |
| 表单填写 | `pdf-core` 新增 Form API + `viewer` 状态 + `ui` 控件 |
| 缩略图/连续多页 | `pdf-core` 增加低分辨率渲染命令 + `ui::page` 改为多页列表 |
| 新的 PDF 后端（MuPDF 等） | 实现与 `PdfEngine` 相同的 Command/Event 契约 |

## 测试

- 纯逻辑（`layout`、`selection`、`search`、`progress`、`home`、`viewer` 的进度恢复、
  `ui::text_input` 的 `TextField`、`highlights` 的 id/删除）有单元测试：
  `cargo test -p gpui-pdf`。
- `dict-core` 有 CSS 层叠、选择器/伪元素、表格、图片、字体路径的单元测试：`cargo test -p dict-core`。
- 真实词典回归测试默认忽略，需要指向《王力古漢語字典》的 `.mdx`：
  ```powershell
  $env:GPUI_PDF_TEST_DICT = 'C:\path\to\王力古漢語字典 (2000).mdx'
  cargo test -p dict-core -- --ignored
  ```
- `pdf-core` 可加用 `samples/` 下 PDF 的集成测试（需要 `pdfium.dll`）。
- GPUI 层目前靠手动验证：`cargo run -p gpui-pdf -- samples/dummy.pdf`。
- 词典样式支持范围与限制见 [dictionary.md](dictionary.md)。
