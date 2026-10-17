# 架构

目标：把“PDF 能力”“阅读器状态”“GPUI 界面”分干净，后面加功能（表单、批注、多页滚动、AI 侧栏……）时不需要改一团乱麻。

## 分层

```
┌─────────────────────────────────────────────┐
│ crates/gpui-pdf（二进制）                     │
│  app.rs      控制器：引擎事件、键盘/鼠标、生命周期 │
│  home.rs     首页状态：最近阅读、搜索、侧栏       │
│  progress.rs 阅读进度模型：页码/滚动比例/缩放     │
│  viewer.rs   阅读器状态：文档/页/缩放/滚动       │
│  layout.rs   适宽/适页缩放、窗口尺寸（纯计算）    │
│  selection.rs / search.rs / highlights.rs     │
│  geometry.rs 纯几何工具                        │
│  ui/*        纯呈现（impl AppState）           │
└───────────────┬─────────────────────────────┘
                │ Command / Event（mpsc channel）
┌───────────────▼─────────────────────────────┐
│ crates/pdf-core（库，无 UI 依赖）              │
│  engine.rs  PDFium worker：打开/渲染/提取       │
│  model.rs   RectPt / CharBox / Link / Outline │
└─────────────────────────────────────────────┘
```

## 各模块职责

| 模块 | 负责 | 不负责 |
|---|---|---|
| `pdf-core::engine` | PDFium 生命周期、线程、渲染成 BGRA、文本/链接/目录提取 | 布局、坐标换算、交互 |
| `pdf-core::model` | UI 无关的数据类型 | 任何 GPUI 类型 |
| `app::AppState` | 引擎事件 → Viewer 更新；键盘/鼠标分派；打开文件 | 具体控件绘制 |
| `home::HomeState` | 最近阅读持久化（含阅读进度）、首页搜索过滤、侧栏选中项 | GPUI 控件 |
| `progress::ReadingProgress` | 页码/页内滚动比例/缩放/适应模式的数据模型与百分比换算 | 磁盘读写、视口换算 |
| `layout` | 页边距、适宽/适页缩放、按纸张比例算窗口尺寸 | 绘制、窗口 API |
| `viewer::Viewer` | 当前文档、页、缩放、滚动、页面缓存，恢复阅读位置，聚合 selection/search/highlights | GPUI 事件 |
| `selection` | 锚点/焦点、矩形合并、复制文本、双击选词 | 绘制 |
| `search` | 查询、匹配区间、当前匹配循环 | 输入法/键盘 |
| `highlights` | 高亮模型、sidecar JSON 读写 | 屏幕坐标 |
| `geometry` | 矩形合并等纯函数 | — |
| `ui::home` | 首页布局：侧栏、顶栏搜索、拖拽区、最近阅读卡片 | 磁盘写入 |
| `ui::chrome` | 顶部/底部浮动胶囊、窗口按钮、拖拽条 | 状态变更 |
| `ui::page` | 滚动容器、页面位图、覆盖层、链接热区 | 命中测试 |
| `ui::outline` | 浮动目录面板 | 书签解析 |
| `ui::empty` / `ui::toast` | 空状态、瞬时提示 | — |

## 数据流

1. UI 触发 `Command::Open(path)` / `Command::Render { page, scale, request }`。
2. `pdf-core` worker 在独立线程处理，回传 `Event::Opened` / `Event::Rendered` / `Event::Error`。
3. `AppState::handle_event` 更新 `Viewer`；打开成功时写入 `HomeState` 的最近阅读并恢复上次的页码/缩放（或适宽/适页模式）/页内滚动，
   `cx.notify()` 触发重绘。打开、切页、缩放、回到首页和退出时把阅读进度落盘；页内滚动由每秒一次的采样兜住。
   打开文档后 `Render` 依据当前显示器工作区调整窗口尺寸；之后每次 `viewport` 变化都按当前适应模式重算缩放。
4. `Render` 里调用 `ui::*` 的方法把 `Viewer` / `HomeState` 映射成 element 树。
5. 首页状态下 `ui::home` 读取 `HomeState`；有文档时改画 `ui::page`，顶部胶囊的「首页」清空 `Viewer` 回到首页。

约束：

- **pdf-core 不依赖 gpui**，可以直接写单元测试，也可以以后换渲染后端。
- **UI 不直接调 PDFium**，只读 Viewer。
- **坐标换算只在一处**：`AppState::page_origin` / `to_pdf`，PDF pt（左下原点）↔ 屏幕 px（左上原点）。
- **PDFium 非线程安全**：所有 PDFium 调用都在 worker 线程；UI 只拿结果。

## 加功能的落点

| 想加什么 | 改哪里 |
|---|---|
| 首页新导航页 / 最近、收藏、设置 | `ui/home.rs` + `home.rs`（侧栏状态） |
| 真实 PDF 首页缩略图 | `pdf-core` 增加缩略图命令 + `ui/home.rs` 缓存 |
| 新的浮动按钮/菜单 | `ui/chrome.rs` + `ui/mod.rs` |
| 新的快捷键 | `app.rs::on_key_down` |
| 新的阅读状态（如连续滚动位置、阅读进度） | `progress.rs`（模型）+ `viewer.rs`（恢复）+ `app.rs`（采样与落盘） |
| 新的文本交互（选区扩展、按词移动） | `selection.rs` + `app.rs` 的鼠标处理 |
| 新的搜索能力（全文、正则） | `search.rs` + `pdf-core`（跨页搜索要新增 Command/Event） |
| 新批注类型（下划线、便签） | `highlights.rs` 泛化为 `annotations.rs` |
| 表单填写 | `pdf-core` 新增 Form API + `viewer` 状态 + `ui` 控件 |
| 缩略图/连续多页 | `pdf-core` 增加低分辨率渲染命令 + `ui::page` 改为多页列表 |
| 新的 PDF 后端（MuPDF 等） | 实现与 `PdfEngine` 相同的 Command/Event 契约 |

## 测试

- 纯逻辑（`layout`、`selection`、`search`、`progress`、`home`、`viewer` 的进度恢复）有单元测试：
  `cargo test -p gpui-pdf`。
- `pdf-core` 可加用 `samples/` 下 PDF 的集成测试（需要 `pdfium.dll`）。
- GPUI 层目前靠手动验证：`cargo run -p gpui-pdf -- samples/dummy.pdf`。
