# 笺释 (gloss-pdf)

**笺释 (`gloss-pdf`)** 是一款基于 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 与 PDFium 构建的极速桌面 PDF 阅读器，专为**古籍文献精读与即时字典训诂**打造。

它采用无边框沉浸界面与 GPU 瓦片渲染，将全部视觉重心留给书页本身；同时内置**独立吸附式 MDX 词典浮窗**，双击页面字词即可按原版词典的 HTML/CSS 与内嵌字体呈现释义。

![《警世通言》阅读与词典窗口](docs/reader-dictionary.png)

---

## 核心特性

### 1. 便捷词典查询（MDX / MDD）
- **双击即查**：双击 PDF 中的单字或词语，或直接在词典浮窗输入（完整支持中文输入法），即刻查询释义。
- **原貌排版渲染**：解析词典自带的 HTML/CSS 与 `@font-face`（支持 WOFF/WOFF2 罕用字与古籍字库），还原《王力古汉语字典》《汉语大词典》等工具书的排版层次。
- **多词典聚合与管理**：支持添加、启停、排序多本 `.mdx` 词典，按顺序聚合展示并标注来源。
- **边缘自动吸附**：词典为独立无边框浮窗，靠近主窗口左右边缘时自动贴合吸附，并随主窗口移动或缩放而同步跟随，不遮挡书页正文。
- **前缀联想与历史**：输入时跨词典实时提供前缀联想（去重、最多 8 条），自动保存最近 50 条查词历史。

### 2. 纯粹的沉浸阅读界面
- **无边框设计**：Windows 自绘左上角控制按钮，macOS 原生集成系统红黄绿交通灯，顶部 46px 透明拖拽区（双击可最大化）。
- **自动隐身胶囊**：阅读器顶部（功能栏）与底部（翻页与缩放栏）采用居中浮动胶囊设计。鼠标离开感应区 1.5 秒后自动隐身，不遮挡任何页面内容，也不拦截点击与滚轮；移回顶部/底部或改变阅读参数时立即唤醒。
- **按纸张比例自适应窗口**：打开文档时根据纸张长宽比自动调整窗口尺寸，横版或竖版古籍都不会在两侧留下大片空白。
- **最近阅读真实缩略图**：首页卡片直接显示上次读到的那一页，而不是示意图；缩略图按文件、页码与修改时间缓存到用户数据目录，第二次启动即可秒开。
- **内置霞鹜文楷**：界面默认嵌入 **LXGW 文楷 TC**，开箱即享温润书卷气。

![首页](docs/home.png)

### 3. 高性能渲染与全文检索
- **GPU 瓦片渲染 + 元素层分离**：页面按 512×512 瓦片在独立工作线程渲染并上传为 GPUI 纹理，缩放与滚动丝滑不卡顿；文字选区、链接、搜索与高亮直接基于 PDF 矢量字符坐标绘制，拼接零错位。
- **翻页优先的渲染队列**：每一帧把可视范围发布给渲染线程，位图永远排在文字提取与全文搜索前面；快速翻页时积压的、已滚出视口的瓦片直接作废，当前页始终最先出图。
- **智能内存预算**：瓦片缓存默认上限 256 MiB，超限时优先淘汰远离可视区域的页面，400% 放大长卷也不会撑爆内存。
- **跨空格/换行全文搜索**：`Ctrl+F` 全文检索自动忽略查询词与 PDF 文本层中的空格、换行等空白字符（如检索「汉字」可精准命中排版拆开的「汉 字」或跨行文本），支持实时扫描进度提示与跨页跳转。
- **高亮批注与自动恢复**：选中文字按 `H` 添加高亮，`Ctrl+S` 保存为同名 `.highlights.json` 旁路文件（不破坏原始 PDF）；阅读页码、页内滚动位置、缩放模式及窗口几何位置均自动持久化，下次打开原样恢复。

---

## 快速上手

### 1. 准备 PDFium 动态库

程序运行依赖 PDFium 动态链接库：
- **Windows**：仓库根目录已内置 `pdfium.dll`，直接可用。
- **macOS**：运行脚本一键下载匹配当前架构（Apple Silicon / Intel）的 `libpdfium.dylib`：
  ```bash
  bash scripts/setup-pdfium.sh
  ```
  *(也可通过 `brew install pdfium` 安装，或手动将 `libpdfium.dylib` 放到项目根目录)*

### 2. 编译与运行

```bash
# 启动到首页
cargo run -p gloss-pdf

# 或直接打开指定 PDF 文件
cargo run -p gloss-pdf -- samples/dummy.pdf
```

> **提示**：首次编译 GPUI 耗时较长。运行时程序按「可执行文件目录 → 当前工作目录 → macOS Frameworks 目录 → 系统/Homebrew 库目录」顺序查找 PDFium 动态库。
>
> 如需替换默认界面字体版本，可将环境变量 `GLOSS_PDF_FONT_DIR` 指向包含 `LXGWWenKaiTC-Regular.ttf` 与 `LXGWWenKaiMonoTC-Regular.ttf` 的目录（重启生效）。

---

## 快捷键一览

| Windows / Linux | macOS | 功能说明 |
| :--- | :--- | :--- |
| `Ctrl+O` | `⌘O` | 打开 PDF 文件 |
| `Ctrl+B` | `⌘B` | 显示 / 隐藏目录浮层 |
| `Ctrl+F` | `⌘F` | 首页筛选最近阅读；阅读器内开启全文搜索 |
| `Ctrl+C` | `⌘C` | 复制选中文本（自动按行重建空格与换行） |
| `H` | `H` | 将当前选区添加为高亮批注 |
| `Ctrl+S` | `⌘S` | 保存高亮批注到磁盘 |
| `Ctrl+H` | `⌘H` | 打开 / 关闭批注列表侧栏 |
| `Ctrl+=` / `Ctrl+-` | `⌘=` / `⌘-` | 放大 / 缩小页面（步进 1%） |
| `Ctrl+滚轮` | `⌘+滚轮` | 以鼠标指针位置为锚点平滑缩放（范围 25%–400%） |
| `Ctrl+0` / `Ctrl+1` | `⌘0` / `⌘1` | 适页（整页可见） / 适宽 |
| `按住中键拖动` | `按住中键拖动` | 平移拖拽页面 |
| `点击底部页码` | `点击底部页码` | 输入目标页码后按 `Enter` 跳转，`Esc` 取消 |
| `PageDown` / `PageUp` | `PageDown` / `PageUp` | 下一页 / 上一页 |
| `Home` / `End` | `Home` / `End` | 跳至第一页 / 最后一页 |
| `Ctrl+D` | `⌘D` | 切换深色 / 浅色主题 |
| `Esc` | `Esc` | 关闭搜索 / 目录 / 设置面板，或清除选区 |
| `Ctrl+Q` | `⌘Q` | 退出程序 |

---

## 打包分发与自动更新

项目支持 **Windows** 与 **macOS** 双端打包及应用内更新检查：
- **Windows**：通过 Inno Setup 生成 `gloss-pdf-<version>-setup.exe`（CI：`.github/workflows/windows.yml`）。
- **macOS**：生成 `gloss-pdf.app` 及 `.dmg` 镜像（CI：`.github/workflows/macos.yml`）。
- **应用内更新**：设置面板支持一键检查 GitHub Releases 最新正式版，下载对应平台安装包并启动安装。

### 发布新版本流程

```bash
# 1. 修改 Cargo.toml 中 [workspace.package] 的 version（CI 会校验 tag 与版本号一致）
# 2. 提交并推送版本标签
git tag v0.1.5
git push origin main --tags
```

### 本地手动打包

- **macOS**（输出至 `dist/` 目录）：
  ```bash
  bash installer/build-macos-app.sh
  ```
- **Windows**（需安装 Inno Setup 6）：
  ```powershell
  cargo build --release -p gloss-pdf
  & "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" `
    "/DAPP_VERSION=0.1.4" "installer\gloss-pdf.iss"
  # 产物：dist\gloss-pdf-0.1.4-setup.exe
  ```

若直接分发免安装便携包，请保留以下文件同目录：
```text
gloss-pdf.exe
pdfium.dll
THIRD_PARTY_NOTICES.md
third_party/                                           # PDFium 及其第三方组件许可
crates/gloss-pdf/assets/fonts/lxgw-wenkai-tc/OFL.txt   # 字体许可声明（SIL OFL 1.1）
```

---

## 数据与配置文件存储

笺释不会修改你的原始 PDF 文件，所有个人数据均独立存储：

| 数据内容 | 存储位置 | 说明 |
| :--- | :--- | :--- |
| **高亮批注** | `<PDF同目录>/<文件名>.pdf.highlights.json` | 随文档旁路保存，带稳定 ID 与选中文本 |
| **最近阅读与进度** | `%APPDATA%\gloss-pdf\recent.json` | 记录路径、打开时间、页码、滚动比例与缩放模式 |
| **应用设置** | `%APPDATA%\gloss-pdf\settings.json` | 主题、多词典路径与启用顺序、自动更新开关、窗口几何 |
| **查词历史** | `%APPDATA%\gloss-pdf\dictionary_history.json` | 最近 50 条查询词（倒序去重） |
| **缩略图缓存** | `%APPDATA%\gloss-pdf\thumbnails\` | 首页最近阅读卡片的页面预览，按文件、页码与修改时间缓存 |

*(注：macOS 下用户数据目录为 `~/Library/Application Support/gloss-pdf`，Linux 下为 `~/.local/share/gloss-pdf`)*

---

## 项目架构与开发指南

工作区按「无 UI 核心引擎 + GPUI 视图层」解耦，便于独立测试与维护：

```text
Cargo.toml                 # Workspace 根配置
crates/
├── pdf-core/              # PDF 引擎（不依赖 UI，独立线程运行 PDFium）
│   └── src/
│       ├── engine.rs      # 打开文档、512×512 瓦片渲染、文本/链接/目录提取、全文检索
│       ├── metadata.rs    # 读取文档元数据与文件尾 /ModDate 兜底
│       └── model.rs       # 几何原语、字符盒、目录树与元数据模型
├── dict-core/             # MDX 词典引擎（不依赖 UI）
│   └── src/
│       ├── engine.rs      # 多词典加载、聚合查询、前缀联想、MDD 资源与外置样式/字体加载
│       ├── css.rs         # lightningcss 样式解析、选择器匹配、层叠与继承
│       ├── html.rs        # HTML 转语义渲染块（段落、行内盒、Flex、表格、图片）
│       └── fonts.rs       # WOFF/WOFF2 解包与字符码位覆盖检测
└── gloss-pdf/             # GPUI 桌面应用（「笺释」主体）
    ├── assets/            # 内嵌 SVG 图标与 LXGW 文楷字体
    └── src/
        ├── main.rs        # 入口：日志初始化、无边框窗口创建、快捷键绑定
        ├── app.rs         # 顶层状态装配与 Render 入口
        ├── app/           # 按职责拆分的控制器模块（document / events / render / window / update）
        ├── viewer.rs      # 阅读器状态、瓦片缓存与 256 MiB 内存淘汰策略
        ├── dictionary.rs  # 词典浮窗状态同步
        └── ui/            # 纯视图呈现组件（首页、胶囊栏、页面、目录、批注、设置、词典窗口等）
```

更详细的设计说明请参阅：
- **架构与模块边界**：[docs/architecture.md](docs/architecture.md)
- **MDX 词典样式支持详情**：[docs/dictionary.md](docs/dictionary.md)
- **功能路线图与待办清单**：[docs/todo.md](docs/todo.md)

### 运行测试

```powershell
cargo test --workspace --locked
```

---

## 开源许可与致谢

本项目以 **MIT OR Apache-2.0** 双许可发布（详见 [LICENSE-MIT](LICENSE-MIT) 与 [LICENSE-APACHE](LICENSE-APACHE)）。

核心第三方组件与字体许可：
- **GPUI** (`zed-industries/zed`)：Apache-2.0
- **PDFium** & **pdfium-render**：BSD-3-Clause / MIT OR Apache-2.0
- **LXGW 文楷 TC**（界面内嵌字体）：SIL Open Font License 1.1
- 完整第三方许可声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) 与 `third_party/pdfium/`。
