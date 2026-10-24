# 词典样式

词典窗口现在按 MDX 自带的 HTML/CSS 渲染，不再把释义压成"纯文本 + 粗体/斜体"。

## 解析栈

| 层 | 负责 | 库 |
|---|---|---|
| HTML | 容错解析、实体、自动闭合、DOM 遍历 | `kuchiki`（`html5ever`） |
| 选择器 | 标签/类/id/属性、后代/子代/相邻/兄弟组合器、`:not`、`:first-of-type`、`:nth-child` 等 | `kuchiki` / Servo `selectors` |
| CSS | 样式表、`@import`/`@media`/`@supports`/`@layer`、`@font-face`、CSS 变量、`calc()`、声明值、`content`、`!important` | `lightningcss` |
| 字体 | WOFF/WOFF2 解包成 sfnt、cmap 覆盖范围、按字符选字体 | `dict-core::fonts` + `wuff` |
| 渲染 | 段落 run、`::before`/`::after`、表格、图片 | GPUI `StyledText` / `div` / `img` |

`dict-core` 只保留"层叠 + 继承 + 转成 UI 无关模型"这部分自有逻辑；语法解析和选择器匹配都交给库。

## 资源查找顺序

1. 优先使用文档实际引用的样式表：MDX 头部 description 的 `<link rel=stylesheet>`、词条内 `<link>`、头部 `StyleSheet`。
2. 完全没有引用时才回退：先找 `词典名.css` / `style.css` / `default.css`，再取 MDD 里排序最靠前的一个 `.css`（不再合并全部 CSS）。
3. 同一个样式表里的 `url()`、`@font-face src`、`@import` 先相对该 CSS 所在目录解析，再回退到 MDX 同目录。
4. 同名 `.mdd`、`词典名.1.mdd` 等拆分资源包（按编号顺序）。

因此 `王力.css`、`hdc2025` 的 `hydcd.css` 这类放在 MDX 旁边的外置样式表，以及放在 `.mdd` 里的 `WL-special.woff`、`wave70.png` 都能找到。CSS 支持 UTF-8（含 BOM）、UTF-16 和 GBK/GB18030。

## 支持的 CSS

- 规则：`@import`（相对路径递归展开、循环检测）、`@media`（按词典窗口 420×800 求值 `screen`/宽度/高度）、`@supports`（按当前支持的属性求值）、`@layer`（按顺序扁平化）。
- 选择器：`tag`、`.class`、`#id`、`[attr]`、`[attr=value]`、`~=`/`|=`/`^=`/`$=`/`*=`、后代/`>`/`+`/`~`、`:not()`、`:first/last-child`、`:first/last-of-type`、`:nth-child(n)`、`:empty`、`:link`、`:visited`。
- 伪元素：`::before` / `::after` 的 `content`，支持字符串、`attr(...)` 拼接；`content: none/""` 不生成文本。
- 文本：`color`（hex/rgb/hsl/命名色）、`background-color`、`font-family`、`font-size`、`font-weight`、`font-style`、`text-decoration`、`line-height`、`text-align`、`text-indent`、`white-space`、`visibility`。
- 值：CSS 变量 `var(--x, fallback)`、`calc()` / `min()` / `max()` / `clamp()`（绝对单位与百分比；相对单位的 calc 暂不支持）、`currentColor`（边框颜色）。
- 盒模型：`margin*`、`padding*`、`border*`、`width`、`height`、`display`（`none`/`inline`/`block`/`inline-block`）、`flex` 系列。
- 行内块：`display:inline-block` 保留宽高、内外边距、边框和背景；`vertical-align: top/middle/bottom/baseline` 映射到行内块的交叉轴对齐。
- 浮动：`float:left/right` 会块级化并做左右对齐近似（不做文字环绕）。
- 图片：`<img src>`、`data:` URI、只有 `id` 的图片（按 `id + 扩展名` 去 MDD 找）。
- 表格：`<table>/<tr>/<td>/<th>`，保留 `colspan`、单元格背景/边框/对齐。

解析完成后会输出覆盖率日志：

```
stylesheet: 254 rules (279 source), at-rules {"media": 12}, unsupported declarations 212, unsupported selectors 2
```

`unsupported declarations` 按属性名计数，可用来判断某本词典主要缺哪类能力。

## 字体

- `@font-face` 的 WOFF / WOFF2 都会解包成 sfnt 后注册到 GPUI。
- 字体会额外扫描词典目录和 MDD 里所有 CSS 的 `@font-face`（只取字体声明，不合并规则），因此只出现在词条内 `<link>` 样式表里的字体也能注册。
- 按每个字符在 `font-family` 列表里找第一个"覆盖该码位"的字体；系统字体没有覆盖信息时按可用处理。
- 这样 `Calibri Light` 显示拉丁、`ZhongHuaSongPlane00` 显示汉字、`WL-super` 显示标点、`WL-special` 显示 ～/❶ 等符号。

## 窗口

- 词典窗口和主窗口一样用 `appears_transparent: true` 隐藏系统标题栏，顶部自绘 40px 标题栏（标题、当前词、关闭按钮）。
- 拖动标题栏可移动窗口；靠近主窗口或显示器工作区边缘 24px 时自动吸附。默认开在主窗口右侧，右侧放不下就放左侧。
- Windows 下 GPUI 没有公开的窗口移动 API，通过 HWND 调 `SetWindowPos` 移动；吸附计算在 `window_layout.rs` 里，不依赖 GPUI。
- 词典窗口被关闭时（✕ / Alt+F4），主窗口工具栏的「词典 ·」状态会同步清空。

## 与浏览器的差距

- 没有完整的 CSS 布局引擎：`float` 只做左右对齐近似、不做文字环绕；`position`、`grid`、`columns`、`transform` 不参与布局。
- `display:inline-flex` 仍按行内近似；`vertical-align` 只对行内块生效，纯文本基线偏移尚未实现。
- `white-space: nowrap` 只做部分处理；`text-emphasis`、`ruby`、`::first-letter` 尚未实现。
- 背景图只用于推断书名波浪线的下划线近似，真正的 `background-image` 不绘制；`border-radius` / 阴影尚未实现。
- 链接点击/跳转已支持（`entry://`、`bword://`、`http(s)`）；`:hover` 等动态伪类不会匹配。
- 行内元素的 `margin/padding` 用空格近似；没有圆角、阴影、outline。
- `<script>` 不执行，依赖 JS 动态插入的内容/交互不会出现。

## 验证

```powershell
cargo test -p dict-core
$env:GLOSS_PDF_TEST_DICT = 'C:\path\to\王力古漢語字典 (2000).mdx'
cargo test -p dict-core -- --ignored
```

真实词典测试会检查外置 CSS、8 个字体、`～`/`❶`/`頁`/`引` 等伪元素内容，以及 1.6em 的词头。
