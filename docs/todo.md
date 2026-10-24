# gloss-pdf 未完成工作清单

阅读体验、互操作与工程化的剩余事项。

> 基线：`main` / `32cdbb9` + 本轮 7 项改动（工作区）
> 更新：2026-10-18 进度更新（跳页、Ctrl+滚轮、元数据、多词典/历史/联想、瓦片与内存预算、release 配置）
> 代码量 18,935 行；`cargo test --workspace --locked` 全绿（gloss-pdf 128 项、dict-core 50 项、pdf-core 15 项，另 2 项联网/真实词典测试忽略）

## 目录

- [一、优先级总览](#一优先级总览)
- [二、当前已交付范围](#二当前已交付范围)
- [三、P1 阅读体验](#三p1-阅读体验)
- [四、P2 互操作与内容能力](#四p2-互操作与内容能力)
- [五、P0 遗留的边界情况](#五p0-遗留的边界情况)
- [六、工程化欠账](#六工程化欠账)
- [七、建议执行顺序](#七建议执行顺序)
- [附录：涉及文件速查](#附录涉及文件速查)

## 一、优先级总览

本文档汇总 gloss-pdf 当前版本已交付范围之外的全部待办事项，按阅读体验、互操作、遗留边界与工程化四类归纳。每条包含现状（已核实的代码位置）、缺口、改动落点与验收标准，可直接作为任务拆分的输入。

当前基线：`main` 已合并 P0 批次与 P1 连续滚动（最新提交 `32cdbb9`），工作区约 18.9k 行，`cargo test --workspace --locked` 全绿。本轮完成：跳页输入、Ctrl+滚轮指针锚定缩放、PDF 元数据、多词典/查词历史/输入联想、512×512 瓦片渲染与 256 MiB 内存预算、release 体积优化，并同步 README/architecture/todo。

**表 1　待办事项优先级总览**

| 状态    | 优先级 | 事项                     | 主要落点                                                       | 性质      |
| ----- | --- | ---------------------- | ---------------------------------------------------------- | ------- |
| ✅ 已完成 | P1  | 连续滚动多页                 | `viewer.rs`、`ui/page.rs`、`progress.rs`                     | 架构改动，最大 |
| ✅ 已完成 | P1  | 跳页输入                   | `ui/chrome.rs`、`app/events.rs`、`ui/text_input.rs`          | 小       |
| ✅ 已完成 | P1  | Ctrl+滚轮指针锚定缩放         | `app/events.rs`、`app/render.rs`、`layout.rs`                | 小，需几何测试 |
| ✅ 已完成 | P1  | PDF 元数据                 | `pdf-core/model.rs`、`pdf-core/metadata.rs`、`ui/settings.rs` | 新增字段与兜底 |
| ✅ 已完成 | P1  | 多词典 + 查词历史 + 输入联想     | `dict-core/engine.rs`、`settings.rs`、`dictionary.rs`         | 跨层改动    |
| ✅ 已完成 | P1  | 512×512 瓦片 + 256 MiB 预算 | `tiles.rs`、`viewer.rs`、`ui/page.rs`、`pdf-core/engine.rs`   | 架构改动    |
| ✅ 已完成 | 工程  | release 体积优化            | `Cargo.toml`                                               | 配置      |
| ✅ 已完成 | 文档  | README / architecture / todo 同步 | 三份文档                                                | 文档      |
| ☐ 待办  | P1  | 真实缩略图                  | `pdf-core`、`ui/home.rs`                                    | 新增命令与缓存 |
| ☐ 待办  | P1  | 目录面板增强                 | `ui/outline.rs`                                            | 局部重写    |
| ☐ 待办  | P1  | 收藏落地                   | `home.rs`、`ui/home.rs`                                     | 新增存储    |
| ☐ 待办  | P2  | 高亮写入标准注释               | `pdf-core`、`highlights.rs`                                 | 需手写外观流  |
| ✅ 已完成 | P2  | 词典渲染三处缺口               | `dict-core/css.rs`、`dict-core/fonts.rs`、`ui/dictionary.rs` | 改动分散    |
| ☐ 待办  | P2  | 表单字段、打印导出              | `pdf-core`、`ui/chrome.rs`                                  | 新增能力    |
| ✅ 已完成 | 低   | 搜索空白容错（所有空格/换行）     | `pdf-core/text_match.rs`、`search.rs`                       | 小，需两侧一致 |
| ✅ 已完成 | 低   | 光标闪烁、全文扫描进度            | `ui/text_input.rs`、`pdf-core/engine.rs`、`ui/search_bar.rs` | 体验打磨    |
| ☐ 待办  | 低   | 批注侧栏增强                 | `ui/annotations.rs`                                        | 体验打磨    |
| ✅ 已完成 | 工程  | `app.rs` 按职责拆分          | `app.rs`、`app/{events,document,render,window}.rs`          | 架构改动    |
| ☐ 待办  | 工程  | 真实 PDF 集成测试            | `crates/pdf-core`                                           | 持续投入    |

## 二、当前已交付范围

P0 与连续滚动完成后，阅读主链路已闭环。连续滚动的实现见第三节（一）；以下 P0 交付的输入、搜索、高亮与词典能力不需要重做，仅需在后续改动中保持，作为后续改动的对照基线。

- 基于 `EntityInputHandler` 的单行输入组件（`ui/text_input.rs`），支持中文输入法组合输入、选区、复制粘贴与横向滚动。
- 阅读器搜索框与首页筛选框共用该组件；`Ctrl+F` 聚焦并全选，`Enter` 与 `Shift+Enter` 跨页跳转。
- 词典窗口可手动输入词条并回车查词，双击 PDF 选词会把词回填到输入框。
- 词典浮窗贴着主窗口左右侧时会跟着主窗口走：主窗口的 `observe_window_bounds` 通知里调 `AppState::follow_dictionary_window`，用 `window_layout::Anchor::place` 按锚点（贴哪边、水平间距、竖直偏移）重新贴边 + `clamp_inside` 夹进工作区，所以拖动、按纸张比例改大小、最大化都不会把浮窗落下；被工作区边缘夹住时锚点不变，主窗口移回来浮窗回到原位。锚点只在两处算：创建时用 `Anchor::from_geometry` 从当时几何读出（竖直有交集且水平间距在 -2–24px，即吸附后的 0 间距与初始摆放的 12px 空隙都算），用户拖动浮窗后由 `DictRequest::FloatPlaced` 把新位置报回主窗口重算：拖远就不再跟随，拖回来自动恢复。移动窗口的 `SetWindowPos` 跑在 `window_move.rs` 的专用线程上（队列只留最后一次请求）：它在主窗口的 bounds 回调里直接调会同步触发浮窗的 `bounds_changed`，那边再借一次 `App` 必然失败（日志刷 `RefCell already borrowed`），窗口内部位置状态也不更新；`cx.defer` / `cx.spawn` 也不行——延迟效果和前台任务同样是拿着 `&mut App` 执行闭包，只有换线程才能让 `WM_MOVE` 落到借用释放之后。
- 高亮带稳定 id 与选中文字，可删除；新增批注侧栏（`Ctrl+H`）按页列出并跳转。
- 跨页全文搜索：worker 逐页提取文本并缓存，界面按「全局序号与页内序号」换算。
- 中文查询有回归测试覆盖（`search.rs` 中三项用例），匹配按字符下标而非字节下标。
- 查询串与文本层里的空白（空格、制表符、换行、全角空格等）都不参与匹配：「汉 字」「T 细胞」「医\n生」分别能被「汉字」「T细胞」「医生」命中，高亮区间覆盖中间空白；页内匹配与全文计数共用 `pdf-core/text_match.rs` 的同一套规则。
- 全文搜索按页回报扫描进度，搜索框显示「已扫描 N/M 页」；输入框光标以 500ms 为半周期闪烁，输入后立即回亮。
- 点击底部页码可输入跳转，`Enter` 夹取到 1..总页数，`Esc` 取消；输入框聚焦时阅读快捷键不误触。
- 缩放按百分点步进：`Ctrl+=` / `Ctrl+-` 与底部胶囊的 ± 按钮每次 1%（先把当前比例对齐到整百分点，适页留下的小数比例也一样），`Ctrl+滚轮` 每行 1%（一格约 3%）；以指针下的页面点为锚缩放，clamp 25%–400%，并退出适宽/适页模式；普通滚轮仍只滚动。
- 按住鼠标中键（滚轮键）拖动可平移页面：指针变抓手光标，位移按 `layout::PAN_RATIO = 0.5` 半速跟随（指针走 2px，内容走 1px），纵横向都夹在内容范围内；中键在容器外松开时下一次移动会自动退出拖动。
- 窗口几何跟着设置走：`settings.json` 里存 `window.{x,y,width,height,maximized}`，启动时按当前显示器工作区恢复（多点显示器取重叠最多的那块，副屏拔掉后自动居中），尺寸夹在 640×520 与工作区之间；尺寸/位置变化后 400ms 去抖落盘，退出时兜底写一次。全屏或 Windows 最小化（-32000 哨兵坐标）不写，避免把好几何覆盖掉。
- PDF 元数据读取 Title/Author/Subject/Keywords/Creator/Producer/CreationDate/ModDate；窗口标题与最近阅读优先 Title，缺失回退文件名；设置面板展示全部字段。
- 多词典：设置中可添加/移除/排序/启停 MDX，查询按顺序聚合并标注来源；每本 MDD/CSS/字体独立；旧 `dictionary_path` 自动迁移。
- 查词历史持久化、倒序、去重、上限 50，可点击重查或清空；输入时跨词典前缀联想，去重、最多 8 条、160ms 防抖，精确前缀无结果时回退近似前缀。
- 大页面按 512×512 瓦片渲染，只请求与视口相交的瓦片；瓦片位图缓存默认 256 MiB，超限按距可视范围由远及近淘汰，400% 浏览多页内存不随页数线性增长。覆盖层仍按整页坐标，文本/链接/选区/高亮不错位。
- `app.rs` 已按职责拆为 `app/{events,document,render,window}.rs`，`app.rs` 只保留状态容器与装配（505 行）。
- 浮动胶囊（顶部按钮条、底部页码/缩放条）自动隐身：判定逻辑全在无 GPUI 依赖的 `chrome_visibility.rs` 里（`band_at` 几何感应带 + `ChromeVisibility` 状态机，13 项单测）。隐藏延迟 `HIDE_DELAY = 1500ms`（在模块顶部，可调），感应带高 `TOP_BAND_HEIGHT = 56.0` / `BOTTOM_BAND_HEIGHT = 64.0`（`Bands::STANDARD`，都比胶囊实际占位略大一圈，鼠标扫过就能唤醒）。唤醒有三条路径，都会重置倒计时：鼠标进入感应带；参数签名变化（`ChromeSignature` 摘要缩放、页码、页数、适宽/适页、各面板开关、主题、搜索开合、高亮数、页码输入框，`PartialEq` 比较，首帧采样不算变化）；以及每帧 `render` 的兜底结算。计时用 `cx.spawn` + `background_executor().timer`，以代数计数器（`generation`）作废过期定时器，旧定时器到期时代数对不上就直接返回，不会把刚显示的胶囊又藏掉。隐藏走 `hidden()`（`display: none`）而不是 `invisible()`：后者仍占布局位置、仍在命中树里，会吞掉页面的点击/滚轮/拖选/中键平移。鼠标位置来自窗口级 `Window::on_mouse_event::<MouseMoveEvent>`，但注册点不能放在 `render` 里：`Render::render` 跑在布局阶段，而 `on_mouse_event` 带 `debug_assert_paint`，在那里注册会 panic `this method can only be called during paint`（panic 展开时还留着借用的 `App`，之后所有 `AsyncApp` 操作都会 `RefCell already borrowed`）。所以挪进 `canvas` 的 paint 回调（`ui/chrome.rs` 的 `chrome_mouse_watch`，作为零尺寸子元素挂在 `root` 上），监听器每帧清空、每帧重注册，并且只吃 `DispatchPhase::Capture` 那一遍（否则同一次移动会被派发两次）。另外：见过第一次鼠标移动之前不开始计时（`ChromeInput::pointer_moved` / `ChromeVisibility::pointer_seen`），否则应用一开屏胶囊就自己藏起来，不用元素级 `on_hover`——顶部 46px 拖拽条是 `WindowControlArea::Drag`，Windows 上是 HTCAPTION 非客户区，元素 hover 收不到，而 `gpui_windows` 会把 `WM_NCMOUSEMOVE` 翻译成同样的 `MouseMoveEvent` 走统一输入管线。页码输入框打开、中键平移、拖选进行时不隐身。

## 三、P1 阅读体验

本节四项决定读长文档时的连贯性：翻页是否跟手、首页能否一眼辨认文档、目录能否定位、入口点了是否有反应。这四项都落在界面层与查看器模型上，不涉及外部协议，但顺序与依赖关系需要安排。

### （一）连续滚动多页 ✅

状态：**已完成**（提交 `28152a7`，合并 `968fb3f`，已推送 `origin/main`）。

原现状是 `viewer.rs` 第 35 行的 `pub page: Option<PageData>` 单页模型：一次只持有一页的位图与字符数据，滚动条只对应页内比例，翻页只能整页跳转。该模型已被文档级页缓存取代。

实现落点：

- `layout.rs` 新增 `DocumentLayout`，按缩放与视口算出每页的 top/left、内容总高与可视页范围，并提供滚动偏移与页内比例的正逆换算；
- `viewer.rs` 改为按页缓存瓦片（页数上限 12，另有默认 256 MiB 位图预算，按距可视范围淘汰），渲染请求带瓦片原点与代号，过期结果直接丢弃；
- `ui/page.rs` 改成单个滚动容器内绝对定位的页面列，只构建可视范围 ±1 页，瓦片按整页坐标拼接，覆盖层按页坐标绘制。

进度格式最终没有升级：页内比例语义沿用，旧记录可直接恢复，因此未写迁移代码。

验证结果：当时 `cargo test` 95 项全绿（新增 15 项，覆盖布局换算与边界、页缓存淘汰、逐页搜索缓存）；12 页样张实测：启动渲染第 1–3 页，滚动 14 格后渲染第 4–6 页、页码由 1/12 变为 4/12，无空白页闪烁；伪造「第 5 页、页内比例 0.42」的进度记录重启后，直接定位并渲染第 5–8 页。

### （二）真实缩略图 ☐

现状：`ui/home.rs` 第 704 行的注释明确写着「卡片缩略图（占位排版，不读 PDF 内容）」，最近阅读卡片的预览是按文件路径稳定选出的示意图，与文档内容无关，无法凭缩略图辨认文档。

落点：`pdf-core` 新增 `Command::Thumbnail`，以低分辨率渲染首页并存到用户数据目录（可复用现有进度存储的目录约定），同一路径加修改时间作为缓存键；首页只读缓存，未命中时先显示占位，结果返回后替换。

验收标准：首次进入首页时卡片逐张替换为真实首页，二次启动直接命中缓存、无渲染延迟。

### （三）目录面板增强 ☐

现状：`ui/outline.rs` 第 48 行只做「按 depth 重复空格再加标题」的缩进列表，没有当前位置标记，不能折叠，也不能按标题筛选。几百条目的长文档目录基本不可用——看不出当前读到哪一章，找某个小节只能硬滚。

落点：加三件事即可，不必重写结构。命中当前页的条目高亮；有子项的标题可折叠（需要 `OutlineItem` 保留父子关系，目前是展平后的 depth，可在 `pdf-core` 侧补一个 parent 索引）；顶部加筛选框，直接复用 `TextInput`。

验收标准：翻页时目录自动滚动并高亮当前章节；输入「第三章」后列表只剩匹配项。

### （四）收藏落地 ☐

现状：`home.rs` 第 53、54 行分别返回「收藏功能即将上线」「工具功能即将上线」，`HomeNav::Favorites` 与 `HomeNav::Tools` 是两个点了没反应的死入口，既没有能力，又给了用户「即将可用」的预期。

落点：二选一。要么实现收藏——`HomeState` 增加 favorites 列表，阅读器顶栏加星标按钮，侧栏按收藏筛选；要么先把两个入口隐藏，不留空按钮。建议实现收藏，其数据结构与最近阅读几乎同构，成本很低。

验收标准：任意文档加星后在侧栏「收藏」可见，重启后仍在；移除星标后同步消失。

## 四、P2 互操作与内容能力

本节三项工作的共同点是「跨出本程序」：批注要能被别的阅读器看见，词典要能渲染原本的排版，表单与打印要接上标准能力。它们的价值在于让生成的内容能被其他工具消费，但实现成本明显高于界面层改动，适合单独排期。

### （一）高亮写入 PDF 标准注释 ☐

现状：高亮只存在于 PDF 旁边的 sidecar JSON（`highlights.rs`），只有本程序能读。原因是 PDFium 的公开 C API 不提供 `FPDFAnnot_GenerateAppearance`，用 `FPDFAnnot` 创建的 Highlight 注释缺少 `/AP` 外观流，其他阅读器不一定渲染。

落点：自己构造 `/AP` 的 Form XObject（矩形填充加 Multiply 混合模式），写进注释字典；或者退一步，把高亮导出成独立的 PDF 副本（`.annotated.pdf`，仓库的忽略规则里已为这个文件名预留了位置）。

验收标准：在第三方阅读器（Acrobat 或浏览器内置阅读器）中打开同一文件，高亮可见且位置正确。

### （二）词典渲染的三处缺口 ✅

**表 2　词典渲染能力缺口**

| 缺口          | 原现状                                                                                     | 处理方式                                                                            |
| ----------- | -------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| WOFF2 字体不支持 | `dict-core/fonts.rs` 第 82 行注释写明「TTF/OTF 原样返回；WOFF 解包成 sfnt；WOFF2 暂不支持」                 | ✅ 改用 `wuff`（纯 Rust，自带 Brotli 与 zlib）统一解包 WOFF/WOFF2，删掉自写的 120 行 WOFF 解析与 `flate2` 依赖 |
| 弹性布局无真实实现   | `css.rs` 的 `parse_display` 把 flex、grid、table 一律映射为块级，`inline-flex`、`inline-grid` 映射为行内 | ✅ `display:flex` 走 `Block::Container`，映射到 GPUI 原生 flex（内置 taffy）；`grid` 仍按块级近似 |
| 释义内链接点不动    | `ui/dictionary.rs` 全文只有两处 `on_click`（标题栏按钮与「查」按钮），没有对释义内词条链接的点击处理                      | ✅ `<a href>` 解析成 `LinkTarget`，随行内样式继承到子片段；界面用 `InteractiveText` 绑定点击区间 |

**实现要点**

- **WOFF2**：`to_sfnt` 变成三条分支——`wOFF` → `wuff::decompress_woff1`、`wOF2` → `wuff::decompress_woff2`、原生 sfnt 原样返回。`cmap_ranges` 与 `FontLibrary` 不变，解出来的就是普通 sfnt。真实 Roboto WOFF2 实测解包成功（31 段 cmap，`a` 命中、`漢` 不命中）。
- **flex**：模型新增 `Block::Container` / `FlexItem` / `FlexStyle`，覆盖 `flex-direction`、`flex-wrap`、`justify-content`、`align-items`、`align-self`、`gap`、`flex` 简写、`flex-grow/shrink/basis`。`html.rs` 把直接子元素各收成一个子项，行内子元素并入匿名项；界面映射到 `.flex_row()/.justify_between()/.items_center()/.gap()` 等方法。横向容器在子项都没声明尺寸时自动均分宽度——双栏对照几乎都不写宽度，均分才不会把右栏挤没。
- **链接**：`entry://`、`bword://` 转成 `LinkTarget::Entry`（百分号解码、去掉 `#fragment`），`http(s)://`、`mailto:` 转成 `Uri`，其余（`#anchor`、`javascript:`）忽略。点击词条走已有的查询通道，外链走 `cx.open_url`。颜色与下划线**以 CSS 为准**：CSS 声明过 `text-decoration` 就照办（王力字典用 `none` 让页码链接融入正文），完全没声明时才补主题强调色 + 下划线。

**验收标准**：加载一本用 WOFF2 字体、带交叉引用的词典，音标字形正常，点击「见某条」能跳到对应词条。

**验证结果**：`cargo test` 全绿（当时 dict-core 48 项，新增链接/装饰/flex/字体格式用例）；用王力字典实测 8 个 WOFF 字体全部解包（`loaded font ... 4561 ranges`）；用合成词典实测三栏 flex（固定 90px + 两个弹性栏）并排、`gap`/`padding` 生效，默认链接为强调色带下划线、CSS `text-decoration:none` 的链接颜色与装饰均按 CSS 渲染，点击「World」成功跳转到目标词条。

### （三）表单字段与打印导出 ☐

现状：`pdf-core` 只有打开、渲染、文本与链接与目录提取、搜索，没有任何表单字段（AcroForm）或打印相关代码。

落点：表单字段需要遍历页面注释、识别控件类型并绘制叠层控件（文本框、复选框、下拉），提交值写回注释字典；打印可先做「导出当前页或全部页面为新 PDF」，比直接接系统打印对话框简单。

验收标准：带文本框的可填写表单可以输入并保存；能导出全部页面为一份新 PDF。

## 五、P0 遗留的边界情况

本节四项不是新功能，是上一批实现留下的明确边界，建议随对应模块的改动一并处理。

### （一）搜索的空白容错 ✅

部分排版软件导出的 PDF，文本层会在汉字之间插入空格或换行；用户从 PDF 里复制出来的词条也常夹带空格、回车。此时搜索「汉字」无法命中「汉 字」，粘上带空格的查询串也一律落空。

**已实现**：匹配规则集中到 `pdf-core/text_match.rs`。`find_matches` 先把查询串里的空白**全部去掉**（空格、制表符、换行、回车、不间断空格、全角空格等，判据是 `char::is_whitespace`），再把被搜文本同样去空白后比对，最后把压缩后的下标映射回原字符位置（高亮区间因此覆盖中间的空格与换行）。规则不再区分语种：拉丁查询「a b」现在也能命中「ab」，因为用户要的是「排除掉所有空格再查」。`search.rs` 的页内匹配与 `pdf-core/engine.rs` 的全文计数都调用这一份实现，不再各留一套。

> **关键约束**：页内匹配（`search.rs`）与 worker 计数（`pdf-core/engine.rs`）必须使用同一套映射，否则全文总数与页内高亮数量会不一致，跨页跳转会定位错位。

**验收标准**：① 文本层为「这 是 汉 字」时搜索「汉字」命中 1 处，高亮从「汉」覆盖到「字」；② 同一页同时存在「汉字」与「汉 字」时命中 2 处；③ 同一页的页内命中数与 `count_matches` 结果相等；④ 查询串中的空白不影响命中：「医 生」「医\n生」均命中「医生」，「T 细胞」命中「T细胞」；⑤ 纯空白查询不产生任何命中。

### （二）光标不闪烁 ✅

`ui/text_input.rs` 的光标原本是静态矩形，全文检索不到任何动画或闪烁逻辑。

**已实现**：以 1000ms 为一个完整周期（显示 500ms、隐藏 500ms）逐帧重绘；只在输入框聚焦时请求下一帧，失焦后自然停止；输入、移动光标、点击定位都会重置相位，保证刚打完字时光标是亮的；系统开启「减少动态效果」时光标常亮。

**验收标准**：① 聚焦输入框后，光标在 2 秒内至少出现一次明暗切换；② 失焦后光标消失且不再产生重绘帧；③ 连续输入时任意时刻截图都能看到光标。

### （三）批注侧栏能力不足 ☐

侧栏目前只能按页列出、点击跳转、删除。缺编辑高亮附带的文字、调整高亮颜色、按关键字筛选、按页码排序。高亮结构里已经有选中文字字段，加备注只需再补一个备注字段与一个行内编辑框。

### （四）全文搜索缺扫描反馈 ✅

worker 逐页提取文本并计数，大文档第一次搜索需要数秒；本轮之前界面只显示「搜索中…」。

**已实现**：worker 在开始时报一次 `0/N`，之后每扫描 8 页发一次 `Event::SearchProgress`（末页必发）；`SearchState` 记录 `scanned/scanned_total`，搜索框显示「已扫描 42/320 页」。过期请求（用户已改查询）的进度直接丢弃。

**验收标准**：① 300 页以上的文档首次搜索时，进度数字单调递增并在结果返回前到达 N；② 中途改查询，旧请求的进度不再刷新界面；③ 结果返回后进度文案被命中数替换，不残留进度。

## 六、工程化欠账

**表 3　工程化事项**

| 事项          | 现状                                    | 建议                                                         |
| ----------- | ------------------------------------- | ---------------------------------------------------------- |
| 持续集成        | ~~仓库根没有 CI 配置，没有任何自动化~~ | ✅ `.github/workflows/windows.yml`：PR 跑测试，tag 构建安装包并发 Release |
| 打包分发        | ~~无 msi 或便携版配置~~ | ✅ Inno Setup 安装包（`installer/gloss-pdf.iss`），带 PDFium 动态库与字体许可；应用内自动更新读同一个 release 资产 |
| 测试覆盖        | `pdf-core` 15 项单元测试，仍无真实 PDF 集成测试；界面层以单测 + 手测为主       | 给 `pdf-core` 加真实 PDF 的打开、瓦片渲染、文本提取、搜索集成测试，样本放 `samples/`        |
| `app.rs` 体量 | ~~已 1929 行，键盘、鼠标、引擎事件、窗口管理、词典窗口全在一个文件~~ | ✅ 已拆为 505 行装配层 + 四个职责模块，边界见下文                         |
| 忽略规则        | ~~忽略文件缺 `.commandcode/` 与 `output/`~~ | ✅ 已完成（`9ed2bde`）                                           |

### （一）`app.rs` 拆分 ✅

1929 行的 `app.rs` 已拆成 505 行的装配层加四个 `impl AppState` 模块。四者共享同一份状态，不引入新的实体或消息通道，拆分只改变代码位置，不改变运行时结构。

**表 4　app 模块边界**

| 模块 | 行数 | 负责 | 不负责（边界） |
| --- | --- | --- | --- |
| `app.rs` | 505 | 字段定义、`new()` 订阅、`Render` 总装、状态栏、主题/设置开关 | 不实现具体业务分支 |
| `app/events.rs` | 709 | PDF/词典事件、键盘、鼠标、滚轮、链接、剪贴板、屏幕坐标 → 页内字符 | 不直接改渲染缓存与窗口，只调用其他模块 |
| `app/document.rs` | 729 | 打开/关闭、最近阅读、进度采样与落盘、高亮 sidecar、检索会话、多词典配置与资源加载 | 不做布局换算、不发渲染请求、不建窗口 |
| `app/render.rs` | 435 | `DocumentLayout` 几何、缩放/适宽适页、可视瓦片预取与内存淘汰、当前页与匹配同步 | 不写盘、不碰 `Window` |
| `app/window.rs` | 219 | 主窗口尺寸调整/恢复、视口度量同步、词典浮窗创建/定位/关闭 | 不读文档内容与渲染缓存 |

边界划分依据：

1. **按副作用类型归位**：写盘与 `Command::Open/Search` 只出现在 `document`，`Command::RenderTile`/`PageElements` 只出现在 `render`，`Window` 操作只出现在 `window`。审计一条链路时，看副作用就能定位文件。
2. **按输入方向分层**：`events` 是唯一把输入翻译成动作的地方；其他模块对外暴露的都是「命令」（`open_recent`、`set_page`、`toggle_dictionary_window`），按钮、快捷键与测试可以走同一条路径。
3. **状态所有权不变**：四个模块都是 `impl AppState`，字段仍只有一份，避免把 `Viewer` / `HomeState` 拆散后出现跨实体同步；模块之间只通过 `pub(super)` 方法调用。
4. **依赖单向**：`events` → {`document`, `render`, `window`}，`app.rs` → 全部；`render` 与 `window` 互不调用，`document` 只借用 `render` 的几何方法和 `window` 的浮窗方法。这样任何一个模块都不会反向依赖另一个模块的私有实现。

拆分后的验证：本轮 `cargo test --workspace --locked` 全绿（gloss-pdf 128 项）；GUI 冒烟启动 `samples/dummy.pdf`，日志按 `gloss_pdf::app::{document,render,window,events}` 分模块输出，打开、瓦片渲染、按纸张调窗、词典字体注册均正常。另用 707 页文档实测扫描进度出现「已扫描 384/707 页」，并用连续截图确认光标亮灭。

## 七、建议执行顺序

1. ✅ 已完成：忽略规则（`9ed2bde`）、Windows CI/安装包/自动更新。
2. ✅ 已完成：拆分 `app.rs`；补齐汉字空格容错、扫描进度、光标闪烁。
3. ✅ 已完成：跳页输入、Ctrl+滚轮指针锚定缩放、PDF 元数据、多词典/历史/联想、512×512 瓦片 + 256 MiB 预算、release 体积优化。
4. 继续 P1 剩余三项：真实缩略图、目录面板、收藏落地。缩略图与目录面板互不依赖可并行；收藏的数据结构与最近阅读同构，成本最低，可顺手做。
5. 继续 P0 遗留：批注侧栏增强（编辑备注、改颜色、筛选、排序），可随批注功能一并做。
6. 最后做互操作：标准注释、表单打印。词典的 WOFF2/布局/链接已在第四节（二）完成；剩下两项都有外部协议依赖，适合单独排期。

## 附录：涉及文件速查

**表 5　涉及文件与关联事项**

| 文件                                      | 关联事项                                  |
| --------------------------------------- | ------------------------------------- |
| `crates/gloss-pdf/src/app.rs`            | 505 行，状态容器、订阅、顶层 `Render`             |
| `crates/gloss-pdf/src/app/events.rs`     | 709 行，事件分派：引擎/词典事件、键盘、鼠标、滚轮、链接 |
| `crates/gloss-pdf/src/app/document.rs`   | 729 行，文档生命周期：打开/关闭、进度、高亮、检索会话、多词典资源 |
| `crates/gloss-pdf/src/app/render.rs`     | 435 行，渲染调度：布局几何、缩放、可视瓦片预取与内存淘汰 |
| `crates/gloss-pdf/src/app/window.rs`     | 219 行，窗口管理：主窗口尺寸、视口度量、词典浮窗 |
| `crates/gloss-pdf/src/tiles.rs`          | 512×512 瓦片网格与可视瓦片计算 |
| `crates/gloss-pdf/src/settings.rs`       | 多词典设置、旧 `dictionary_path` 迁移 |
| `crates/gloss-pdf/src/dict_history.rs`   | 查词历史持久化（上限 50） |
| `crates/gloss-pdf/src/dict_suggest.rs`   | 联想去重/限长/防抖 |
| `crates/pdf-core/src/metadata.rs`       | `/ModDate` 等文件尾元数据兜底 |
| `crates/gloss-pdf/src/viewer.rs`         | 瓦片缓存、字节预算、连续滚动与锚点恢复            |
| `crates/gloss-pdf/src/layout.rs`         | `DocumentLayout`：每页位置、可视范围、指针/页内比例锚点正逆换算 |
| `crates/gloss-pdf/src/ui/page.rs`        | 页面渲染与覆盖层坐标系                           |
| `crates/gloss-pdf/src/progress.rs`       | 阅读进度的持久化模型                            |
| `crates/gloss-pdf/src/ui/home.rs`        | 缩略图占位（第 704 行）、首页布局                   |
| `crates/gloss-pdf/src/home.rs`           | 收藏与工具的死入口（第 53、54 行）                  |
| `crates/gloss-pdf/src/ui/outline.rs`     | 目录面板（第 48 行）                          |
| `crates/gloss-pdf/src/highlights.rs`     | 高亮模型与 sidecar 读写                      |
| `crates/gloss-pdf/src/search.rs`         | 页内匹配适配层与全文命中换算                        |
| `crates/pdf-core/src/text_match.rs`     | 页内匹配与全文计数共用的匹配规则（含汉字空格容错）             |
| `crates/pdf-core/src/engine.rs`         | 渲染、文本提取、全文计数、搜索命令与扫描进度              |
| `crates/gloss-pdf/src/ui/text_input.rs`  | 输入框与光标闪烁                              |
| `crates/gloss-pdf/src/ui/search_bar.rs`  | 搜索框状态文案（扫描进度/命中数）                     |
| `crates/dict-core/src/css.rs`           | flex 属性的解析与计算样式                       |
| `crates/dict-core/src/fonts.rs`         | WOFF/WOFF2 解包（`wuff`）与 cmap 覆盖范围      |
| `crates/dict-core/src/html.rs`          | `<a href>` → `LinkTarget`、flex 容器 → `Block::Container` |
| `crates/dict-core/src/model.rs`         | `Container`/`FlexStyle`、`SpanStyle.link`  |
| `crates/gloss-pdf/src/ui/dictionary.rs`  | flex 映射到 GPUI、`InteractiveText` 链接点击 |

---

gloss-pdf 项目内部文档 · 依据 `main` 分支 `32cdbb9` + 本轮工作区改动核实（2026-10-18 进度更新）
