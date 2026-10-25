//! MDX 释义 HTML → 块/行内结构。
//!
//! HTML 解析和选择器匹配交给 `kuchiki`（html5ever + Servo selectors），
//! 这里负责层叠结果 → 模型、伪元素内容、表格、图片和字体按字符回退。

use std::sync::Arc;

use kuchiki::traits::TendrilSink;
use kuchiki::{ElementData, NodeData, NodeDataRef, NodeRef};

use crate::css::{self, Computed, Display, PseudoElement, ROOT_SIZE, Stylesheet};
use crate::fonts::FontLibrary;
use crate::model::{
    Align, AlignItems, Block, BlockKind, BlockStyle, Container, FlexDirection, FlexItem, FlexStyle,
    FlexWrap, ImageFormat, ImageSpan, InlineBox, Justify, LinkTarget, Paragraph, Span, SpanStyle,
    Table, TableCell, TableRow, TextSpan,
};

/// 单次释义最多接受多少个块，防止异常词条拖慢界面。
const MAX_BLOCKS: usize = 400;
/// 单次释义最多渲染多少张图片。
const MAX_IMAGES: usize = 24;
/// 单张图片的字节上限。
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// 单次释义所有图片的字节上限。
const MAX_IMAGE_TOTAL_BYTES: usize = 16 * 1024 * 1024;

/// 外部资源（`.mdd` / 词典目录里的样式表、图片）的读取接口。
pub trait ResourceResolver {
    fn stylesheet(&mut self, href: &str) -> Option<String>;
    fn resource(&mut self, path: &str) -> Option<Arc<[u8]>>;
}

/// 不提供任何外部资源；用于纯 HTML 的单元测试。
pub struct NoResources;

impl ResourceResolver for NoResources {
    fn stylesheet(&mut self, _href: &str) -> Option<String> {
        None
    }

    fn resource(&mut self, _path: &str) -> Option<Arc<[u8]>> {
        None
    }
}

/// 渲染入口参数。
pub struct RenderContext<'a> {
    /// 词典级样式表（来自 `.mdd`、词典目录或 MDX 头部）。
    pub stylesheet: &'a Stylesheet,
    pub resolver: &'a mut dyn ResourceResolver,
    /// 自定义字体的覆盖范围；`None` 表示只用 CSS 字体列表的第一个。
    pub fonts: Option<&'a FontLibrary>,
    /// 已经在全局样式表里加载过的外置 CSS，避免词条 `<link>` 重复应用。
    pub loaded_stylesheets: &'a [String],
}

/// 把一段释义 HTML 渲染成块结构。
pub fn render(input: &str, context: &mut RenderContext<'_>) -> Vec<Block> {
    let document = kuchiki::parse_html().one(input);

    let mut custom_stylesheet: Option<Stylesheet> = None;
    if input.contains("<style") || input.contains("<link") {
        if let Ok(styles) = document.select("style") {
            for style in styles {
                let css = style.as_node().text_contents();
                custom_stylesheet
                    .get_or_insert_with(|| context.stylesheet.clone())
                    .extend(Stylesheet::parse(&css));
            }
        }
        if let Ok(links) = document.select("link") {
            for link in links {
                let attributes = link.attributes.borrow();
                let Some(href) = attributes.get("href") else {
                    continue;
                };
                let rel = attributes
                    .get("rel")
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if (rel.contains("stylesheet") || href.to_ascii_lowercase().ends_with(".css"))
                    && context
                        .loaded_stylesheets
                        .iter()
                        .any(|loaded| same_stylesheet(loaded, href))
                {
                    continue;
                }
                if (rel.contains("stylesheet") || href.to_ascii_lowercase().ends_with(".css"))
                    && let Some(css) = context.resolver.stylesheet(href)
                {
                    custom_stylesheet
                        .get_or_insert_with(|| context.stylesheet.clone())
                        .extend(Stylesheet::parse(&css));
                }
            }
        }
    }

    let stylesheet: &Stylesheet = custom_stylesheet.as_ref().unwrap_or(context.stylesheet);

    let mut converter = Converter {
        stylesheet: &stylesheet,
        resolver: context.resolver,
        fonts: context.fonts,
        blocks: Vec::new(),
        spans: Vec::new(),
        ctx: None,
        pending_space: false,
        lists: Vec::new(),
        images: 0,
        image_bytes: 0,
        link: None,
    };

    let mut inherited = Computed::default();
    inherited.size = Some(ROOT_SIZE);
    let ctx = ParaCtx::default();
    for child in document.children() {
        converter.visit(&child, &inherited, &ctx);
    }
    converter.flush();
    converter.blocks.truncate(MAX_BLOCKS);
    converter.blocks
}

/// 没有外部资源的便捷入口，测试和降级路径用。
pub fn to_blocks(input: &str) -> Vec<Block> {
    let stylesheet = Stylesheet::default();
    let mut resolver = NoResources;
    let mut context = RenderContext {
        stylesheet: &stylesheet,
        resolver: &mut resolver,
        fonts: None,
        loaded_stylesheets: &[],
    };
    render(input, &mut context)
}

fn same_stylesheet(left: &str, right: &str) -> bool {
    fn normalize(value: &str) -> String {
        let value = value.trim().replace('\\', "/").to_ascii_lowercase();
        value
            .trim_start_matches('/')
            .trim_start_matches("./")
            .to_string()
    }
    normalize(left) == normalize(right)
}

/// 从释义开头找音标：优先等宽片段，其次任意 `[...]`、`/.../`、`(...)`。
pub fn extract_phonetic(blocks: &[Block]) -> Option<String> {
    let mut fallback = None;
    for block in blocks.iter().take(4) {
        let Block::Paragraph(paragraph) = block else {
            continue;
        };
        for span in &paragraph.spans {
            let Span::Text(span) = span else {
                continue;
            };
            for token in span.text.split_whitespace() {
                if token.is_empty() || token.chars().count() > 64 {
                    continue;
                }
                let first = token.chars().next();
                let last = token.chars().last();
                if !matches!(
                    (first, last),
                    (Some('['), Some(']')) | (Some('/'), Some('/')) | (Some('('), Some(')'))
                ) {
                    continue;
                }
                if span.style.mono {
                    return Some(token.to_string());
                }
                fallback.get_or_insert_with(|| token.to_string());
            }
        }
    }
    fallback
}

// ---------------------------------------------------------------------------
// DOM → 模型
// ---------------------------------------------------------------------------

struct Converter<'a> {
    stylesheet: &'a Stylesheet,
    resolver: &'a mut dyn ResourceResolver,
    fonts: Option<&'a FontLibrary>,
    blocks: Vec<Block>,
    spans: Vec<Span>,
    ctx: Option<ParaCtx>,
    pending_space: bool,
    lists: Vec<ListState>,
    images: usize,
    image_bytes: usize,
    /// 当前所在的 `<a href>` 目标；子节点的行内片段都继承它。
    link: Option<LinkTarget>,
}

#[derive(Clone)]
struct ParaCtx {
    kind: BlockKind,
    style: BlockStyle,
}

impl Default for ParaCtx {
    fn default() -> Self {
        Self {
            kind: BlockKind::Paragraph,
            style: BlockStyle::default(),
        }
    }
}

struct ListState {
    ordered: bool,
    index: u16,
}

const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "center",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "html",
    "li",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

const SKIP_TAGS: &[&str] = &[
    "script", "style", "link", "meta", "title", "head", "base", "iframe", "object", "embed",
    "video", "audio", "canvas", "svg", "param", "source", "track",
];

fn is_block_tag(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

impl Converter<'_> {
    fn visit(&mut self, node: &NodeRef, inherited: &Computed, ctx: &ParaCtx) {
        match node.data() {
            NodeData::Element(data) => {
                let element = match node.clone().into_element_ref() {
                    Some(element) => element,
                    None => return,
                };
                self.visit_element(node, &element, data, inherited, ctx);
            }
            NodeData::Text(text) => {
                let text = text.borrow().clone();
                self.push_text(&text, inherited, ctx);
            }
            _ => {}
        }
    }

    fn visit_element(
        &mut self,
        node: &NodeRef,
        element: &NodeDataRef<ElementData>,
        data: &ElementData,
        inherited: &Computed,
        ctx: &ParaCtx,
    ) {
        let tag = data.name.local.as_ref().to_ascii_lowercase();
        if SKIP_TAGS.contains(&tag.as_str()) {
            return;
        }
        let attrs = attributes(data);
        let computed = self.style_for(&tag, element, &attrs, inherited);
        if matches!(computed.display, Some(Display::None)) || computed.hidden {
            return;
        }
        let before = self.pseudo_style(element, &computed, PseudoElement::Before);
        let after = self.pseudo_style(element, &computed, PseudoElement::After);

        if tag == "br" {
            self.flush();
            return;
        }
        if tag == "img" {
            self.push_image(&attrs, &computed, ctx);
            return;
        }

        // `<a>` 的 href 要传给所有后代片段，让它们共享同一个可点击目标。
        let saved_link = self.link.clone();
        if tag == "a"
            && let Some(target) = attr(&attrs, "href").and_then(parse_link)
        {
            self.link = Some(target);
        }

        let display = if computed.float.is_some() {
            // CSS 的 float 会让元素块级化；这里不做文字环绕，只保留左右对齐。
            Display::Block
        } else {
            computed.display.unwrap_or_else(|| {
                if is_block_tag(&tag) {
                    Display::Block
                } else {
                    Display::Inline
                }
            })
        };
        match display {
            Display::Flex => self.visit_flex(node, &attrs, &computed),
            Display::Block => self.visit_block(node, &tag, &attrs, &computed, before, after),
            Display::None => {}
            Display::InlineBlock => {
                self.visit_inline_block(node, &attrs, &computed, before, after, ctx)
            }
            Display::Inline => {
                self.push_inline_gap(&computed, ctx, false);
                self.push_pseudo(before.as_ref(), &attrs, ctx, false);
                for child in node.children() {
                    self.visit(&child, &computed, ctx);
                }
                self.push_pseudo(after.as_ref(), &attrs, ctx, true);
                self.push_inline_gap(&computed, ctx, true);
            }
        }

        self.link = saved_link;
    }

    fn style_for(
        &self,
        tag: &str,
        element: &NodeDataRef<ElementData>,
        attrs: &[(String, String)],
        inherited: &Computed,
    ) -> Computed {
        let mut computed = Computed::inherited(inherited);
        apply_presentational(&mut computed, tag, attrs);
        apply_tag_defaults(&mut computed, tag);

        let matched = self.stylesheet.matched(element);
        let inline = attr(attrs, "style")
            .map(css::parse_inline_style)
            .unwrap_or_default();
        computed.apply(&matched, &inline, ROOT_SIZE);
        apply_class_defaults(&mut computed, attrs);
        computed
    }

    fn pseudo_style(
        &self,
        element: &NodeDataRef<ElementData>,
        inherited: &Computed,
        pseudo: PseudoElement,
    ) -> Option<Computed> {
        let matched = self.stylesheet.matched_pseudo(element, pseudo);
        if matched.is_empty() {
            return None;
        }
        let mut computed = Computed::inherited(inherited);
        computed.display = None;
        computed.content = None;
        computed.apply(&matched, &[], ROOT_SIZE);
        Some(computed)
    }

    fn visit_block(
        &mut self,
        node: &NodeRef,
        tag: &str,
        attrs: &[(String, String)],
        computed: &Computed,
        before: Option<Computed>,
        after: Option<Computed>,
    ) {
        self.flush();

        match tag {
            "hr" => {
                self.blocks.push(Block::Paragraph(Paragraph {
                    kind: BlockKind::Rule,
                    style: block_style(computed, None),
                    spans: Vec::new(),
                }));
            }
            "table" => self.visit_table(node, computed),
            "ul" | "ol" => {
                self.lists.push(ListState {
                    ordered: tag == "ol",
                    index: 0,
                });
                let ctx = ParaCtx {
                    kind: BlockKind::Paragraph,
                    style: block_style(computed, None),
                };
                for child in node.children() {
                    self.visit(&child, computed, &ctx);
                }
                self.lists.pop();
                self.flush();
            }
            "li" => {
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    Some(list) => {
                        list.index = list.index.saturating_add(1);
                        if list.ordered {
                            format!("{}.", list.index)
                        } else {
                            "•".to_string()
                        }
                    }
                    None => "•".to_string(),
                };
                let mut style = block_style(computed, Some(marker));
                style.indent += 14.0 * depth.saturating_sub(1) as f32;
                style.margin_top.get_or_insert(2.0);
                style.margin_bottom.get_or_insert(2.0);
                let ctx = ParaCtx {
                    kind: BlockKind::ListItem,
                    style,
                };
                self.push_pseudo(before.as_ref(), attrs, &ctx, false);
                for child in node.children() {
                    self.visit(&child, computed, &ctx);
                }
                self.push_pseudo(after.as_ref(), attrs, &ctx, true);
                self.flush();
            }
            "pre" => {
                let ctx = ParaCtx {
                    kind: BlockKind::Paragraph,
                    style: block_style(computed, None),
                };
                self.push_pseudo(before.as_ref(), attrs, &ctx, false);
                for child in node.children() {
                    self.visit(&child, computed, &ctx);
                }
                self.push_pseudo(after.as_ref(), attrs, &ctx, true);
                self.flush();
            }
            "blockquote" => {
                let ctx = ParaCtx {
                    kind: BlockKind::Quote,
                    style: block_style(computed, None),
                };
                self.push_pseudo(before.as_ref(), attrs, &ctx, false);
                for child in node.children() {
                    self.visit(&child, computed, &ctx);
                }
                self.push_pseudo(after.as_ref(), attrs, &ctx, true);
                self.flush();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = tag[1..].parse::<u8>().unwrap_or(6);
                let ctx = ParaCtx {
                    kind: BlockKind::Heading(level),
                    style: block_style(computed, None),
                };
                self.push_pseudo(before.as_ref(), attrs, &ctx, false);
                for child in node.children() {
                    self.visit(&child, computed, &ctx);
                }
                self.push_pseudo(after.as_ref(), attrs, &ctx, true);
                self.flush();
            }
            _ => {
                let ctx = ParaCtx {
                    kind: BlockKind::Paragraph,
                    style: block_style(computed, None),
                };
                self.push_pseudo(before.as_ref(), attrs, &ctx, false);
                for child in node.children() {
                    self.visit(&child, computed, &ctx);
                }
                self.push_pseudo(after.as_ref(), attrs, &ctx, true);
                self.flush();
            }
        }
    }

    fn visit_inline_block(
        &mut self,
        node: &NodeRef,
        attrs: &[(String, String)],
        computed: &Computed,
        before: Option<Computed>,
        after: Option<Computed>,
        ctx: &ParaCtx,
    ) {
        let blocks = self.capture_cell(node, attrs, computed, before, after);
        self.ensure_ctx(ctx);
        self.spans.push(Span::Box(InlineBox {
            style: block_style(computed, None),
            blocks,
        }));
    }

    /// `display: flex` 容器：直接子元素各成一个 flex 子项，行内子元素并进匿名项。
    fn visit_flex(&mut self, node: &NodeRef, _attrs: &[(String, String)], computed: &Computed) {
        self.flush();
        let flex = flex_style(computed);
        let style = block_style(computed, None);

        let saved_blocks = std::mem::take(&mut self.blocks);
        let saved_spans = std::mem::take(&mut self.spans);
        let saved_ctx = self.ctx.take();
        let saved_lists = std::mem::take(&mut self.lists);
        let saved_pending = self.pending_space;
        self.pending_space = false;

        let ctx = ParaCtx {
            kind: BlockKind::Paragraph,
            style: style.clone(),
        };
        let mut items: Vec<FlexItem> = Vec::new();
        let mut anonymous: Vec<Block> = Vec::new();
        let anonymous_style = BlockStyle {
            margin_top: Some(0.0),
            margin_bottom: Some(0.0),
            ..Default::default()
        };

        for child in node.children() {
            let Some(data) = child.as_element() else {
                // 纯文本直接进匿名项，和相邻的行内元素连成一段。
                self.visit(&child, computed, &ctx);
                continue;
            };
            let tag = data.name.local.as_ref().to_ascii_lowercase();
            if SKIP_TAGS.contains(&tag.as_str()) {
                continue;
            }
            let Some(element) = child.clone().into_element_ref() else {
                continue;
            };
            let child_attrs = attributes(data);
            // `<img>` 不是块级元素，直接当行内内容并进匿名项，别让 capture 丢掉它。
            if tag == "img" {
                self.visit(&child, computed, &ctx);
                continue;
            }
            let child_computed = self.style_for(&tag, &element, &child_attrs, computed);
            if matches!(child_computed.display, Some(Display::None)) || child_computed.hidden {
                continue;
            }
            let child_display = child_computed.display.unwrap_or_else(|| {
                if is_block_tag(&tag) {
                    Display::Block
                } else {
                    Display::Inline
                }
            });

            if matches!(child_display, Display::Inline) {
                self.push_inline_gap(&child_computed, &ctx, false);
                let before = self.pseudo_style(&element, &child_computed, PseudoElement::Before);
                let after = self.pseudo_style(&element, &child_computed, PseudoElement::After);
                let saved_link = self.link.clone();
                if tag == "a"
                    && let Some(target) = attr(&child_attrs, "href").and_then(parse_link)
                {
                    self.link = Some(target);
                }
                self.push_pseudo(before.as_ref(), &child_attrs, &ctx, false);
                for grand in child.children() {
                    self.visit(&grand, &child_computed, &ctx);
                }
                self.push_pseudo(after.as_ref(), &child_attrs, &ctx, true);
                self.link = saved_link;
                self.push_inline_gap(&child_computed, &ctx, true);
                continue;
            }

            self.flush();
            anonymous.extend(std::mem::take(&mut self.blocks));
            if !anonymous.is_empty() {
                items.push(FlexItem {
                    style: anonymous_style.clone(),
                    blocks: std::mem::take(&mut anonymous),
                });
            }

            let before = self.pseudo_style(&element, &child_computed, PseudoElement::Before);
            let after = self.pseudo_style(&element, &child_computed, PseudoElement::After);
            let blocks = self.capture_cell(&child, &child_attrs, &child_computed, before, after);
            if !blocks.is_empty() {
                items.push(FlexItem {
                    style: block_style(&child_computed, None),
                    blocks,
                });
            }
        }

        self.flush();
        anonymous.extend(std::mem::take(&mut self.blocks));
        if !anonymous.is_empty() {
            items.push(FlexItem {
                style: anonymous_style,
                blocks: anonymous,
            });
        }

        self.blocks = saved_blocks;
        self.spans = saved_spans;
        self.ctx = saved_ctx;
        self.lists = saved_lists;
        self.pending_space = saved_pending;

        if !items.is_empty() {
            self.blocks
                .push(Block::Container(Container { style, flex, items }));
        }
    }

    fn visit_table(&mut self, node: &NodeRef, computed: &Computed) {
        let style = block_style(computed, None);
        let mut rows = Vec::new();
        self.collect_rows(node, computed, &mut rows);
        if rows.is_empty() {
            let ctx = ParaCtx {
                kind: BlockKind::Paragraph,
                style,
            };
            for child in node.children() {
                self.visit(&child, computed, &ctx);
            }
            self.flush();
        } else {
            self.blocks.push(Block::Table(Table { style, rows }));
        }
    }

    fn collect_rows(&mut self, node: &NodeRef, inherited: &Computed, rows: &mut Vec<TableRow>) {
        for child in node.children() {
            let Some(data) = child.as_element() else {
                continue;
            };
            let tag = data.name.local.as_ref().to_ascii_lowercase();
            if SKIP_TAGS.contains(&tag.as_str()) {
                continue;
            }
            let Some(element) = child.clone().into_element_ref() else {
                continue;
            };
            let attrs = attributes(data);
            let computed = self.style_for(&tag, &element, &attrs, inherited);
            if tag == "tr" {
                self.collect_row(&child, &computed, rows);
            } else if matches!(tag.as_str(), "thead" | "tbody" | "tfoot" | "table") {
                self.collect_rows(&child, &computed, rows);
            }
        }
    }

    fn collect_row(&mut self, node: &NodeRef, computed: &Computed, rows: &mut Vec<TableRow>) {
        let row_style = block_style(computed, None);
        let mut cells = Vec::new();
        for child in node.children() {
            let Some(data) = child.as_element() else {
                continue;
            };
            let tag = data.name.local.as_ref().to_ascii_lowercase();
            if tag != "td" && tag != "th" {
                continue;
            }
            let Some(element) = child.clone().into_element_ref() else {
                continue;
            };
            let attrs = attributes(data);
            let cell_computed = self.style_for(&tag, &element, &attrs, computed);
            let before = self.pseudo_style(&element, &cell_computed, PseudoElement::Before);
            let after = self.pseudo_style(&element, &cell_computed, PseudoElement::After);
            let colspan = attr(&attrs, "colspan")
                .and_then(|value| value.trim().parse::<u16>().ok())
                .unwrap_or(1)
                .max(1);
            let blocks = self.capture_cell(&child, &attrs, &cell_computed, before, after);
            cells.push(TableCell {
                style: block_style(&cell_computed, None),
                colspan,
                blocks,
            });
        }
        if !cells.is_empty() {
            rows.push(TableRow {
                style: row_style,
                cells,
            });
        }
    }

    fn capture_cell(
        &mut self,
        node: &NodeRef,
        attrs: &[(String, String)],
        computed: &Computed,
        before: Option<Computed>,
        after: Option<Computed>,
    ) -> Vec<Block> {
        let saved_blocks = std::mem::take(&mut self.blocks);
        let saved_spans = std::mem::take(&mut self.spans);
        let saved_ctx = self.ctx.take();
        let saved_lists = std::mem::take(&mut self.lists);
        let saved_pending = self.pending_space;
        self.pending_space = false;

        let ctx = ParaCtx {
            kind: BlockKind::Paragraph,
            style: block_style(computed, None),
        };
        self.push_pseudo(before.as_ref(), attrs, &ctx, false);
        for child in node.children() {
            self.visit(&child, computed, &ctx);
        }
        self.push_pseudo(after.as_ref(), attrs, &ctx, true);
        self.flush();

        let blocks = std::mem::take(&mut self.blocks);
        self.blocks = saved_blocks;
        self.spans = saved_spans;
        self.ctx = saved_ctx;
        self.lists = saved_lists;
        self.pending_space = saved_pending;
        blocks
    }

    fn ensure_ctx(&mut self, ctx: &ParaCtx) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
    }

    /// 行内样式：CSS 计算值加上当前所在的链接目标。
    fn span_style(&self, computed: &Computed) -> SpanStyle {
        let mut style = base_span_style(computed);
        style.link = self.link.clone();
        style
    }

    fn push_text(&mut self, text: &str, computed: &Computed, ctx: &ParaCtx) {
        if text.is_empty() {
            return;
        }
        self.ensure_ctx(ctx);
        let base = self.span_style(computed);
        let families = computed.font_family.as_deref();
        let mut current = base.clone();
        let mut current_family: Option<String> = None;
        let preserve = computed.preserve_space;

        for ch in text.chars() {
            if preserve {
                if ch == '\n' || ch == '\r' {
                    self.flush();
                    self.ensure_ctx(ctx);
                } else {
                    self.push_char(if ch == '\t' { ' ' } else { ch }, &current);
                }
                continue;
            }
            if ch.is_whitespace() || ch == '\u{a0}' {
                self.pending_space = true;
                continue;
            }
            if self.pending_space {
                self.push_space(&current);
                self.pending_space = false;
            }
            if let Some(families) = families {
                let family = self.choose_family(families, ch);
                if family.as_deref() != current_family.as_deref() {
                    current_family = family.map(|family| family.to_string());
                    current.family = current_family.clone();
                }
            }
            self.push_char(ch, &current);
        }
    }

    fn choose_family<'f>(&self, families: &'f [String], ch: char) -> Option<&'f str> {
        let Some(fonts) = self.fonts else {
            return families.first().map(String::as_str);
        };
        for family in families {
            match fonts.covers(family, ch) {
                Some(true) => return Some(family),
                Some(false) => continue,
                // 系统字体没有覆盖范围信息，按可用处理。
                None => return Some(family),
            }
        }
        families.first().map(String::as_str)
    }

    fn push_pseudo(
        &mut self,
        computed: Option<&Computed>,
        attrs: &[(String, String)],
        ctx: &ParaCtx,
        after: bool,
    ) {
        let Some(computed) = computed else {
            return;
        };
        if matches!(computed.display, Some(Display::None)) || computed.hidden {
            return;
        }
        let Some(content) = &computed.content else {
            return;
        };
        let text = content.resolve(attrs);
        if text.is_empty() {
            return;
        }
        self.ensure_ctx(ctx);
        let base = self.span_style(computed);
        let families = computed.font_family.as_deref();
        let mut current = base.clone();
        let mut current_family: Option<String> = None;

        if after && (computed.margin_left.unwrap_or(0.0) > 0.0 || computed.padding.left > 0.0) {
            self.push_space(&current);
        }
        for ch in text.chars() {
            let ch = if ch == '\n' || ch == '\r' || ch == '\t' {
                ' '
            } else {
                ch
            };
            if let Some(families) = families {
                let family = self.choose_family(families, ch);
                if family.as_deref() != current_family.as_deref() {
                    current_family = family.map(|family| family.to_string());
                    current.family = current_family.clone();
                }
            }
            self.push_char(ch, &current);
        }
        if !after && (computed.margin_right.unwrap_or(0.0) > 0.0 || computed.padding.right > 0.0) {
            self.push_space(&current);
        }
    }

    fn push_char(&mut self, ch: char, style: &SpanStyle) {
        if let Some(Span::Text(last)) = self.spans.last_mut()
            && last.style == *style
        {
            last.text.push(ch);
            return;
        }
        self.spans.push(Span::Text(TextSpan {
            text: ch.to_string(),
            style: style.clone(),
        }));
    }

    fn push_space(&mut self, style: &SpanStyle) {
        if self.spans.is_empty() {
            return;
        }
        match self.spans.last_mut() {
            Some(Span::Text(last)) => {
                if !last.text.ends_with(' ') {
                    last.text.push(' ');
                }
            }
            _ => self.push_char(' ', style),
        }
    }

    /// 行内元素的外边距/内边距用一个空格近似，避免 `lǐlejX` 这种粘连。
    fn push_inline_gap(&mut self, computed: &Computed, ctx: &ParaCtx, after: bool) {
        let gap = if after {
            computed.margin_right.unwrap_or(0.0) + computed.padding.right
        } else {
            computed.margin_left.unwrap_or(0.0) + computed.padding.left
        };
        let font_size = computed.size.unwrap_or(ROOT_SIZE);
        if gap > font_size * 0.2 {
            self.ensure_ctx(ctx);
            let style = self.span_style(computed);
            self.push_space(&style);
        }
    }

    fn push_image(&mut self, attrs: &[(String, String)], computed: &Computed, ctx: &ParaCtx) {
        self.ensure_ctx(ctx);
        if self.pending_space {
            let style = self.span_style(computed);
            self.push_space(&style);
            self.pending_space = false;
        }

        let alt = attr(attrs, "alt")
            .or_else(|| attr(attrs, "title"))
            .unwrap_or_default()
            .to_string();
        let src = attr(attrs, "src")
            .map(str::trim)
            .filter(|src| !src.is_empty());
        if self.images >= MAX_IMAGES {
            self.push_alt(&alt, computed);
            return;
        }
        let bytes = src
            .and_then(|src| self.load_resource(src))
            .or_else(|| self.load_id_resource(attrs));
        let Some(bytes) = bytes else {
            self.push_alt(&alt, computed);
            return;
        };
        if bytes.len() > MAX_IMAGE_BYTES || self.image_bytes + bytes.len() > MAX_IMAGE_TOTAL_BYTES {
            self.push_alt(&alt, computed);
            return;
        }
        let Some(format) = ImageFormat::sniff(&bytes) else {
            self.push_alt(&alt, computed);
            return;
        };
        let Some((width, height)) = image_dimensions(&bytes, format) else {
            self.push_alt(&alt, computed);
            return;
        };

        self.images += 1;
        self.image_bytes += bytes.len();
        self.spans.push(Span::Image(ImageSpan {
            data: Arc::from(bytes),
            format,
            width,
            height,
            display_width: computed.width.or_else(|| attr_dimension(attrs, "width")),
            display_height: computed.height.or_else(|| attr_dimension(attrs, "height")),
            alt,
        }));
    }

    fn push_alt(&mut self, alt: &str, computed: &Computed) {
        let mut style = self.span_style(computed);
        style.italic = true;
        let text = if alt.is_empty() { "[图片]" } else { alt };
        for ch in text.chars() {
            self.push_char(ch, &style);
        }
    }

    fn load_resource(&mut self, src: &str) -> Option<Vec<u8>> {
        if let Some(data) = src.strip_prefix("data:") {
            return decode_data_uri(data);
        }
        self.resolver.resource(src).map(|bytes| bytes.to_vec())
    }

    /// 有些词典的 `<img>` 只有 `id`（例如 `TY339`），图片由脚本按 id 从 MDD 取。
    fn load_id_resource(&mut self, attrs: &[(String, String)]) -> Option<Vec<u8>> {
        let id = attr(attrs, "id").or_else(|| attr(attrs, "name"))?;
        for extension in ["png", "jpg", "jpeg", "gif", "webp", "bmp"] {
            let path = format!("{id}.{extension}");
            if let Some(bytes) = self.resolver.resource(&path) {
                return Some(bytes.to_vec());
            }
        }
        None
    }

    fn flush(&mut self) {
        self.pending_space = false;
        let Some(ctx) = self.ctx.take() else {
            self.spans.clear();
            return;
        };
        if let Some(Span::Text(last)) = self.spans.last_mut() {
            let trimmed = last.text.trim_end();
            if trimmed.len() != last.text.len() {
                last.text = trimmed.to_string();
            }
        }
        self.spans.retain(|span| match span {
            Span::Text(text) => !text.text.is_empty(),
            Span::Image(_) => true,
            Span::Box(inline) => !inline.blocks.iter().all(Block::is_empty),
        });
        if self.spans.is_empty() {
            return;
        }
        if self.blocks.len() >= MAX_BLOCKS {
            self.spans.clear();
            return;
        }
        self.blocks.push(Block::Paragraph(Paragraph {
            kind: ctx.kind,
            style: ctx.style,
            spans: std::mem::take(&mut self.spans),
        }));
    }
}

fn attributes(data: &ElementData) -> Vec<(String, String)> {
    data.attributes
        .borrow()
        .map
        .iter()
        .map(|(name, attribute)| (name.local.as_ref().to_string(), attribute.value.clone()))
        .collect()
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn base_span_style(computed: &Computed) -> SpanStyle {
    let border_bottom = computed.border_bottom.filter(|border| border.width > 0.0);
    SpanStyle {
        bold: computed.bold,
        italic: computed.italic,
        underline: computed.underline || border_bottom.is_some() || computed.background_image,
        underline_color: border_bottom.and_then(|border| border.color),
        strike: computed.strike,
        mono: computed.mono,
        size: computed.size,
        color: computed.color,
        background: computed.background,
        family: None,
        link: None,
        decorated: computed.decoration_explicit,
    }
}

fn block_style(computed: &Computed, marker: Option<String>) -> BlockStyle {
    BlockStyle {
        size: computed.size,
        line_height: computed.line_height,
        color: computed.color,
        family: computed.font_family.clone(),
        align: computed.align,
        margin_top: computed.margin_top,
        margin_bottom: computed.margin_bottom,
        margin_left: computed.margin_left,
        margin_right: computed.margin_right,
        padding: computed.padding,
        indent: computed.indent.unwrap_or(0.0),
        background: computed.background,
        border_top: computed.border_top,
        border_right: computed.border_right,
        border_bottom: computed.border_bottom,
        border_left: computed.border_left,
        width: computed.width,
        height: computed.height,
        marker,
        float: computed.float,
        vertical_align: computed.vertical_align,
        grow: computed.flex_grow,
        shrink: computed.flex_shrink,
        basis: computed.flex_basis,
        align_self: computed.align_self,
    }
}

fn flex_style(computed: &Computed) -> FlexStyle {
    FlexStyle {
        direction: computed.flex_direction.unwrap_or(FlexDirection::Row),
        wrap: computed.flex_wrap.unwrap_or(FlexWrap::NoWrap),
        justify: computed.justify.unwrap_or(Justify::Start),
        align_items: computed.align_items.unwrap_or(AlignItems::Stretch),
        gap: computed.gap.unwrap_or(0.0),
    }
}

/// 把 `<a href>` 归一化成链接目标；认不出的协议直接忽略。
fn parse_link(href: &str) -> Option<LinkTarget> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return Some(LinkTarget::Uri(href.to_string()));
    }
    let rest = lower
        .starts_with("entry://")
        .then(|| &href["entry://".len()..])
        .or_else(|| {
            lower
                .starts_with("bword://")
                .then(|| &href["bword://".len()..])
        })?;
    let rest = rest.split('#').next().unwrap_or_default();
    let term = percent_decode(rest);
    let term = term.trim();
    (!term.is_empty()).then(|| LinkTarget::Entry(term.to_string()))
}

fn apply_presentational(computed: &mut Computed, tag: &str, attrs: &[(String, String)]) {
    if let Some(value) = attr(attrs, "align") {
        match value.to_ascii_lowercase().as_str() {
            "left" => computed.align = Some(Align::Left),
            "center" | "middle" => computed.align = Some(Align::Center),
            "right" => computed.align = Some(Align::Right),
            _ => {}
        }
    }
    if tag == "font" {
        if let Some(color) = attr(attrs, "color").and_then(css::parse_color) {
            computed.color = Some(color);
        }
        if let Some(face) = attr(attrs, "face")
            && let Some(family) = css::parse_family_list(face)
        {
            computed.font_family = Some(family);
        }
        if let Some(size) = attr(attrs, "size").and_then(font_size_attribute) {
            computed.size = Some(size);
        }
    }
    if let Some(color) = attr(attrs, "bgcolor").and_then(css::parse_color) {
        computed.background = Some(color);
    }
    if let Some(width) = attr_dimension(attrs, "width") {
        computed.width = Some(width);
    }
    if let Some(height) = attr_dimension(attrs, "height") {
        computed.height = Some(height);
    }
}

fn apply_tag_defaults(computed: &mut Computed, tag: &str) {
    match tag {
        "b" | "strong" => computed.bold = true,
        "i" | "em" | "cite" | "var" | "dfn" => computed.italic = true,
        "u" | "ins" => computed.underline = true,
        "s" | "strike" | "del" => computed.strike = true,
        "code" | "tt" | "kbd" | "samp" => computed.mono = true,
        "pre" => {
            computed.mono = true;
            computed.preserve_space = true;
        }
        "th" => {
            computed.bold = true;
            computed.align = Some(Align::Center);
        }
        "center" => computed.align = Some(Align::Center),
        _ => {}
    }
}

fn apply_class_defaults(computed: &mut Computed, attrs: &[(String, String)]) {
    if computed.font_family.is_some() {
        return;
    }
    let Some(class) = attr(attrs, "class") else {
        return;
    };
    let class = class.to_ascii_lowercase();
    if class.contains("phonetic") || class.contains("pron") || class.contains("ipa") {
        computed.mono = true;
    }
}

fn font_size_attribute(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Ok(number) = value.parse::<i32>() {
        let sizes = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
        let index = (number.clamp(1, 7) - 1) as usize;
        return Some(sizes[index]);
    }
    None
}

fn attr_dimension(attrs: &[(String, String)], name: &str) -> Option<f32> {
    let value = attr(attrs, name)?.trim();
    if value.is_empty() || value.ends_with('%') {
        return None;
    }
    if let Ok(number) = value.parse::<f32>() {
        return Some(number);
    }
    css::parse_length(value)
        .and_then(|length| length.fixed())
        .map(|value| value.max(0.0))
}

fn image_dimensions(bytes: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    let format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
    };
    image::ImageReader::with_format(std::io::Cursor::new(bytes), format)
        .into_dimensions()
        .ok()
}

fn decode_data_uri(data: &str) -> Option<Vec<u8>> {
    let (meta, payload) = data.split_once(',')?;
    let payload = payload.trim();
    if meta.to_ascii_lowercase().contains("base64") {
        decode_base64(payload)
    } else {
        Some(percent_decode(payload).into_bytes())
    }
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }

    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() || byte == b'=' {
            continue;
        }
        let value = value(byte)?;
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(value) = u8::from_str_radix(&input[index + 1..index + 3], 16)
        {
            output.push(value);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AlignSelf, Color, FloatSide};

    struct MemoryResolver {
        files: std::collections::HashMap<String, Vec<u8>>,
    }

    impl ResourceResolver for MemoryResolver {
        fn stylesheet(&mut self, href: &str) -> Option<String> {
            self.files
                .get(href)
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        }

        fn resource(&mut self, path: &str) -> Option<Arc<[u8]>> {
            self.files.get(path).map(|bytes| Arc::from(bytes.clone()))
        }
    }

    fn text(blocks: &[Block]) -> String {
        blocks
            .iter()
            .map(Block::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn paragraph(block: &Block) -> &Paragraph {
        match block {
            Block::Paragraph(paragraph) => paragraph,
            Block::Table(_) | Block::Container(_) => panic!("expected paragraph"),
        }
    }

    #[test]
    fn splits_paragraphs_and_decodes_entities() {
        let blocks = to_blocks("<p>hello &amp; world</p><p>second<br>line</p>");
        assert_eq!(blocks.len(), 3);
        assert_eq!(paragraph(&blocks[0]).text(), "hello & world");
        assert_eq!(paragraph(&blocks[1]).text(), "second");
        assert_eq!(paragraph(&blocks[2]).text(), "line");
    }

    #[test]
    fn keeps_inline_bold_and_italic() {
        let blocks = to_blocks("<p>a <b>bold</b> and <i>italic</i> end</p>");
        assert_eq!(paragraph(&blocks[0]).text(), "a bold and italic end");
        let spans = &paragraph(&blocks[0]).spans;
        assert!(spans.iter().any(
            |span| matches!(span, Span::Text(text) if text.style.bold && text.text.contains("bold"))
        ));
        assert!(spans.iter().any(|span| matches!(span, Span::Text(text) if text.style.italic && text.text.contains("italic"))));
    }

    #[test]
    fn external_stylesheet_applies() {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "dict.css".to_string(),
            b".hwd { color: #0000ff; font-size: 22px; } .def { color: #333333; }".to_vec(),
        );
        let mut resolver = MemoryResolver { files };
        let mut stylesheet = Stylesheet::default();
        let mut context = RenderContext {
            stylesheet: &stylesheet,
            resolver: &mut resolver,
            fonts: None,
            loaded_stylesheets: &[],
        };
        let blocks = render(
            r#"<link rel="stylesheet" href="dict.css">
               <div class="hwd">word</div>
               <div class="def">meaning</div>"#,
            &mut context,
        );
        assert_eq!(paragraph(&blocks[0]).style.size, Some(22.0));
        assert_eq!(
            paragraph(&blocks[0]).style.color,
            Some(Color::rgb(0, 0, 255))
        );
        assert_eq!(
            paragraph(&blocks[1]).style.color,
            Some(Color::rgb(51, 51, 51))
        );
        stylesheet.extend(Stylesheet::default());
    }

    #[test]
    fn pseudo_elements_produce_text() {
        let blocks = to_blocks(
            r#"<style>
                 w::before { content: "" attr(w) ""; }
                 p[n]::before { content: attr(n); }
               </style>
               <p n="❶">祭神<w w="禮"></w>致福。</p>"#,
        );
        assert_eq!(text(&blocks), "❶祭神禮致福。");
    }

    #[test]
    fn inline_style_and_class_rules_apply() {
        let blocks = to_blocks(
            r#"<style>.def { color: #ff0000; font-weight: bold; }</style>
               <p class="def" style="font-size: 18px">hi</p>"#,
        );
        let style = &paragraph(&blocks[0]).style;
        assert_eq!(style.color, Some(Color::rgb(255, 0, 0)));
        assert_eq!(style.size, Some(18.0));
        assert!(
            paragraph(&blocks[0])
                .spans
                .iter()
                .any(|span| matches!(span, Span::Text(text) if text.style.bold))
        );
    }

    #[test]
    fn display_none_hides_subtree() {
        let blocks = to_blocks(r#"<div>keep<span style="display:none">drop</span></div>"#);
        assert_eq!(text(&blocks), "keep");
    }

    #[test]
    fn marks_phonetic_span_as_mono() {
        let blocks = to_blocks(r#"<span class="phonetic">[ˈæpl]</span> apple"#);
        assert_eq!(extract_phonetic(&blocks).as_deref(), Some("[ˈæpl]"));
        assert!(paragraph(&blocks[0]).spans.iter().any(
            |span| matches!(span, Span::Text(text) if text.style.mono && text.text.contains("ˈæpl"))
        ));
    }

    #[test]
    fn headings_and_lists_keep_kind() {
        let blocks = to_blocks("<h2>Title</h2><ul><li>one</li><li>two</li></ul>");
        assert_eq!(paragraph(&blocks[0]).kind, BlockKind::Heading(2));
        assert_eq!(paragraph(&blocks[1]).kind, BlockKind::ListItem);
        assert_eq!(paragraph(&blocks[2]).kind, BlockKind::ListItem);
        assert_eq!(paragraph(&blocks[1]).style.marker.as_deref(), Some("•"));
    }

    #[test]
    fn ordered_lists_number_items() {
        let blocks = to_blocks("<ol><li>one</li><li>two</li></ol>");
        assert_eq!(paragraph(&blocks[0]).style.marker.as_deref(), Some("1."));
        assert_eq!(paragraph(&blocks[1]).style.marker.as_deref(), Some("2."));
    }

    #[test]
    fn numeric_entities_are_decoded() {
        let blocks = to_blocks("<p>&#20320;&#x597d;</p>");
        assert_eq!(paragraph(&blocks[0]).text(), "你好");
    }

    #[test]
    fn external_stylesheet_and_images_resolve() {
        let png = decode_base64(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        let mut files = std::collections::HashMap::new();
        files.insert(
            "dict.css".to_string(),
            b".hwd { color: #0000ff; font-size: 22px; } .def { color: #333333; }".to_vec(),
        );
        files.insert("img/dot.png".to_string(), png);
        let mut resolver = MemoryResolver { files };

        let mut stylesheet = Stylesheet::default();
        stylesheet.extend(Stylesheet::parse(
            ".def { color: #333333; } table { border: 1px solid #ccc; }",
        ));
        let mut context = RenderContext {
            stylesheet: &stylesheet,
            resolver: &mut resolver,
            fonts: None,
            loaded_stylesheets: &[],
        };
        let blocks = render(
            r#"<link rel="stylesheet" href="dict.css">
               <div class="hwd">word</div>
               <div class="def">meaning <img src="img/dot.png" width="20"></div>
               <table><tr><td>A</td><td>B</td></tr></table>"#,
            &mut context,
        );

        assert_eq!(paragraph(&blocks[0]).style.size, Some(22.0));
        assert!(paragraph(&blocks[1])
            .spans
            .iter()
            .any(|span| matches!(span, Span::Image(image) if image.width == 1 && image.display_width == Some(20.0))));
        match blocks.last().unwrap() {
            Block::Table(table) => {
                assert_eq!(table.rows.len(), 1);
                assert_eq!(table.rows[0].cells.len(), 2);
                assert_eq!(table.rows[0].cells[0].blocks[0].text(), "A");
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn data_uri_images_decode() {
        let blocks = to_blocks(
            r#"<p>x <img src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=="></p>"#,
        );
        assert!(
            paragraph(&blocks[0])
                .spans
                .iter()
                .any(|span| matches!(span, Span::Image(_)))
        );
    }

    #[test]
    fn missing_image_falls_back_to_alt_text() {
        let blocks = to_blocks(r#"<p>x <img src="missing.png" alt="图"> y</p>"#);
        assert_eq!(paragraph(&blocks[0]).text(), "x 图 y");
    }

    #[test]
    fn image_with_only_id_resolves_from_resources() {
        let png = decode_base64(
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==",
        )
        .unwrap();
        let mut files = std::collections::HashMap::new();
        files.insert("TY339.png".to_string(), png);
        let mut resolver = MemoryResolver { files };
        let stylesheet = Stylesheet::default();
        let mut context = RenderContext {
            stylesheet: &stylesheet,
            resolver: &mut resolver,
            fonts: None,
            loaded_stylesheets: &[],
        };
        let blocks = render(r#"<p><img id="TY339"></p>"#, &mut context);
        assert!(
            paragraph(&blocks[0])
                .spans
                .iter()
                .any(|span| matches!(span, Span::Image(image) if image.width == 1))
        );
    }

    #[test]
    fn loaded_stylesheets_are_not_applied_twice() {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "dict.css".to_string(),
            b".hwd { font-size: 1.6em; }".to_vec(),
        );
        let mut resolver = MemoryResolver { files };
        let stylesheet = Stylesheet::parse(".hwd { font-size: 1.6em; }");
        let loaded = vec!["dict.css".to_string()];
        let mut context = RenderContext {
            stylesheet: &stylesheet,
            resolver: &mut resolver,
            fonts: None,
            loaded_stylesheets: &loaded,
        };
        let blocks = render(
            r#"<link rel="stylesheet" href="dict.css"><div class="hwd">x</div>"#,
            &mut context,
        );
        let size = paragraph(&blocks[0]).style.size.unwrap_or_default();
        assert!((size - 25.6).abs() < 0.1, "{size}");
    }

    fn link_of<'a>(blocks: &'a [Block], text: &str) -> &'a Option<LinkTarget> {
        paragraph(&blocks[0])
            .spans
            .iter()
            .find_map(|span| match span {
                Span::Text(span) if span.text.contains(text) => Some(&span.style.link),
                _ => None,
            })
            .expect("span with text")
    }

    #[test]
    fn anchor_href_becomes_link_target() {
        let blocks = to_blocks(
            r#"<p>见<a href="entry://禮">禮</a>与<a href="https://example.com/x">外链</a>。</p>"#,
        );
        assert_eq!(
            link_of(&blocks, "禮"),
            &Some(LinkTarget::Entry("禮".to_string()))
        );
        assert_eq!(
            link_of(&blocks, "外链"),
            &Some(LinkTarget::Uri("https://example.com/x".to_string()))
        );
        assert!(link_of(&blocks, "见").is_none());
    }

    #[test]
    fn anchor_href_is_decoded_and_stripped() {
        let blocks = to_blocks(
            r##"<p><a href="entry://%E7%A6%AE#top">a</a><a href="#local">b</a>
               <a href="bword://WLK">c</a></p>"##,
        );
        assert_eq!(
            link_of(&blocks, "a"),
            &Some(LinkTarget::Entry("禮".to_string()))
        );
        assert!(link_of(&blocks, "b").is_none());
        assert_eq!(
            link_of(&blocks, "c"),
            &Some(LinkTarget::Entry("WLK".to_string()))
        );
    }

    #[test]
    fn link_decoration_follows_css() {
        let plain = to_blocks(r#"<p><a href="entry://x">x</a></p>"#);
        let Span::Text(plain_span) = &paragraph(&plain[0]).spans[0] else {
            panic!("expected text span");
        };
        assert!(!plain_span.style.decorated, "no css declaration");
        assert!(!plain_span.style.underline);

        let muted = to_blocks(
            r#"<style>a { color: #114488; text-decoration: none; }</style>
               <p><a href="entry://x">x</a></p>"#,
        );
        let span = &paragraph(&muted[0]).spans[0];
        let Span::Text(span) = span else {
            panic!("expected text span");
        };
        assert!(span.style.decorated);
        assert!(!span.style.underline);
        assert_eq!(
            span.style.color,
            Some(crate::model::Color::rgb(17, 68, 136))
        );
    }

    #[test]
    fn inline_block_becomes_an_atomic_box() {
        let blocks = to_blocks(
            r#"<div>a<span style="display:inline-block; width:20px; height:10px;
                                  margin-left:3px; vertical-align:middle">b</span>c</div>"#,
        );
        let Block::Paragraph(paragraph) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(paragraph.spans.len(), 3);
        let Span::Box(inline) = &paragraph.spans[1] else {
            panic!("expected inline box: {:?}", paragraph.spans[1]);
        };
        assert_eq!(inline.style.width, Some(20.0));
        assert_eq!(inline.style.height, Some(10.0));
        assert_eq!(inline.style.margin_left, Some(3.0));
        assert_eq!(inline.style.vertical_align, Some(AlignSelf::Center));
        assert_eq!(inline.blocks[0].text(), "b");
    }

    #[test]
    fn float_becomes_a_block_with_alignment_style() {
        let blocks = to_blocks(r#"<div style="float:right">导航</div><div>正文</div>"#);
        assert_eq!(blocks.len(), 2);
        let Block::Paragraph(nav) = &blocks[0] else {
            panic!("expected float paragraph");
        };
        assert_eq!(nav.style.float, Some(FloatSide::Right));
        assert_eq!(nav.text(), "导航");
        assert_eq!(blocks[1].text(), "正文");
    }

    #[test]
    fn flex_container_builds_items_and_layout() {
        let blocks = to_blocks(
            r#"<div style="display:flex; flex-direction:column; gap:6px;
                           justify-content:space-between; align-items:center">
                 <div class="left">A</div>
                 <div class="right" style="flex:1">B</div>
               </div>"#,
        );
        let Block::Container(container) = &blocks[0] else {
            panic!("expected container");
        };
        assert_eq!(container.flex.direction, FlexDirection::Column);
        assert_eq!(container.flex.gap, 6.0);
        assert_eq!(container.flex.justify, Justify::SpaceBetween);
        assert_eq!(container.flex.align_items, AlignItems::Center);
        assert_eq!(container.items.len(), 2);
        assert_eq!(container.items[0].blocks[0].text(), "A");
        assert_eq!(container.items[1].blocks[0].text(), "B");
        assert_eq!(container.items[1].style.grow, Some(1.0));
        assert_eq!(blocks[0].text(), "A\nB");
    }

    #[test]
    fn flex_row_keeps_inline_siblings_in_one_item() {
        let blocks = to_blocks(
            r#"<div style="display:flex">
                 <span>a</span><span>b</span>
                 <div>c</div>
               </div>"#,
        );
        let Block::Container(container) = &blocks[0] else {
            panic!("expected container");
        };
        assert_eq!(container.items.len(), 2);
        assert_eq!(container.items[0].blocks[0].text(), "ab");
        assert_eq!(container.items[1].blocks[0].text(), "c");
    }

    #[test]
    fn flex_basis_and_auto_grow() {
        let blocks = to_blocks(
            r#"<div style="display:flex"><div style="flex:0 0 120px">x</div><div>y</div></div>"#,
        );
        let Block::Container(container) = &blocks[0] else {
            panic!("expected container");
        };
        assert_eq!(container.items[0].style.grow, Some(0.0));
        assert_eq!(container.items[0].style.shrink, Some(0.0));
        assert_eq!(container.items[0].style.basis, Some(120.0));
        assert_eq!(container.items[1].style.grow, None);
    }
}
