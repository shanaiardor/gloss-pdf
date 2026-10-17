# 词典样式

词典窗口现在按 MDX 自带的 HTML/CSS 渲染，不再把释义压成"纯文本 + 粗体/斜体"。

## 解析栈

| 层 | 负责 | 库 |
|---|---|---|
| HTML | 容错解析、实体、自动闭合、DOM 遍历 | `kuchiki`（`html5ever`） |
| 选择器 | 标签/类/id/属性、后代/子代/相邻/兄弟组合器、`:not`、`:first-of-type`、`:nth-child` 等 | `kuchiki` / Servo `selectors` |
| CSS | 样式表、`@font-face`、声明值、`content`、`!important` | `lightningcss` |
| 字体 | WOFF 解包成 sfnt、cmap 覆盖范围、按字符选字体 | `dict-core::fonts` + `flate2` |
| 渲染 | 段落 run、`::before`/`::after`、表格、图片 | GPUI `StyledText` / `div` / `img` |

`dict-core` 只保留"层叠 + 继承 + 转成 UI 无关模型"这部分自有逻辑；语法解析和选择器匹配都交给库。

## 资源查找顺序

1. MDX 同目录（`<link href="...">`、`<img src="...">`、`@font-face` 的 `url()`）。
2. 同名 `.mdd`。
3. `词典名.1.mdd`、`词典名.2.mdd` 等拆分资源包（按编号顺序）。

因此 `王力.css` 这种放在 MDX 旁边的外置样式表、以及放在 `.mdd` 里的 `WL-special.woff`、`wave70.png` 都能找到。

## 支持的 CSS

- 选择器：`tag`、`.class`、`#id`、`[attr]`、`[attr=value]`、`~=`/`|=`/`^=`/`$=`/`*=`、后代/`>`/`+`/`~`、`:not()`、`:first/last-child`、`:first/last-of-type`、`:nth-child(n)`、`:empty`、`:link`、`:visited`。
- 伪元素：`::before` / `::after` 的 `content`，支持字符串、`attr(...)` 拼接；`content: none/""` 不生成文本。
- 文本：`color`、`background-color`、`font-family`、`font-size`、`font-weight`、`font-style`、`text-decoration`、`line-height`、`text-align`、`text-indent`、`white-space`。
- 盒模型：`margin*`、`padding*`、`border*`、`width`、`height`、`display`（`none`/`inline`/`block`/`inline-block`）。
- 图片：`<img src>`、`data:` URI、只有 `id` 的图片（按 `id + 扩展名` 去 MDD 找）。
- 表格：`<table>/<tr>/<td>/<th>`，保留 `colspan`、单元格背景/边框/对齐。

## 字体

- `@font-face` 的 WOFF1 会解包成 sfnt 后注册到 GPUI；WOFF2 暂不支持。
- 按每个字符在 `font-family` 列表里找第一个"覆盖该码位"的字体；系统字体没有覆盖信息时按可用处理。
- 这样 `Calibri Light` 显示拉丁、`ZhongHuaSongPlane00` 显示汉字、`WL-super` 显示标点、`WL-special` 显示 ～/❶ 等符号。

## 窗口

- 词典窗口和主窗口一样用 `appears_transparent: true` 隐藏系统标题栏，顶部自绘 40px 标题栏（标题、当前词、关闭按钮）。
- 拖动标题栏可移动窗口；靠近主窗口或显示器工作区边缘 24px 时自动吸附。默认开在主窗口右侧，右侧放不下就放左侧。
- Windows 下 GPUI 没有公开的窗口移动 API，通过 HWND 调 `SetWindowPos` 移动；吸附计算在 `window_layout.rs` 里，不依赖 GPUI。
- 词典窗口被关闭时（✕ / Alt+F4），主窗口工具栏的「词典 ·」状态会同步清空。

## 与浏览器的差距

- 没有完整的 CSS 布局引擎：`float`、`flex`、`grid`、`columns`、`position`、`transform` 不参与布局。
- 背景图只用于推断书名波浪线的下划线近似，真正的 `background-image` 不绘制。
- `@media` 规则整块跳过，按桌面宽度渲染。
- 链接不处理点击/跳转，`hover` 样式不生效（`:hover` 选择器不会匹配）。
- 行内元素的 `margin/padding` 用空格近似；没有圆角、阴影、outline 和文本内图片。
- `<script>` 不执行，依赖 JS 动态插入的图片/交互不会出现。

## 验证

```powershell
cargo test -p dict-core
$env:GPUI_PDF_TEST_DICT = 'C:\path\to\王力古漢語字典 (2000).mdx'
cargo test -p dict-core -- --ignored
```

真实词典测试会检查外置 CSS、8 个字体、`～`/`❶`/`頁`/`引` 等伪元素内容，以及 1.6em 的词头。
