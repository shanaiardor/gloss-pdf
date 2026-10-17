//! CSS 样式表的解析、层叠与计算样式。
//!
//! 语法解析交给 `lightningcss`，选择器匹配交给 `kuchiki`（Servo `selectors`），
//! 这里只保留 MDX 释义需要的属性子集、层叠顺序和继承逻辑。

use std::sync::Arc;

use kuchiki::{ElementData, NodeDataRef, Selectors, Specificity};
use lightningcss::declaration::DeclarationBlock;
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use lightningcss::traits::ToCss;

use crate::model::{Align, Border, Color, Edges};

/// CSS 的初始字号；用于 `rem` 以及没有 css 字号时的 `em`。
pub(crate) const ROOT_SIZE: f32 = 16.0;

/// 一份样式表；`extend` 用于把词条内嵌样式接在全局样式后面。
#[derive(Clone, Default)]
pub struct Stylesheet {
    rules: Vec<CompiledRule>,
    font_faces: Vec<FontFace>,
}

#[derive(Clone)]
struct CompiledRule {
    selectors: Vec<CompiledSelector>,
    declarations: Vec<Declaration>,
    order: u32,
}

#[derive(Clone)]
struct CompiledSelector {
    matcher: Arc<Selectors>,
    specificity: Specificity,
    pseudo: Option<PseudoElement>,
}

/// `@font-face` 里声明的一个字体来源。
#[derive(Clone, Debug, PartialEq)]
pub struct FontFace {
    pub family: String,
    pub sources: Vec<String>,
}

impl Stylesheet {
    pub fn parse(css: &str) -> Self {
        let mut stylesheet = Self::default();
        let parsed = match StyleSheet::parse(
            css,
            ParserOptions {
                error_recovery: true,
                ..Default::default()
            },
        ) {
            Ok(parsed) => parsed,
            Err(error) => {
                log::warn!("CSS 解析失败: {error:?}");
                return stylesheet;
            }
        };

        for rule in &parsed.rules.0 {
            match rule {
                CssRule::Style(style) => {
                    let selectors = compile_selectors(&style.selectors);
                    if selectors.is_empty() {
                        continue;
                    }
                    let declarations = convert_declarations(&style.declarations);
                    if declarations.is_empty() {
                        continue;
                    }
                    stylesheet.rules.push(CompiledRule {
                        selectors,
                        declarations,
                        order: stylesheet.rules.len() as u32,
                    });
                }
                CssRule::FontFace(face) => {
                    if let Ok(text) = face.to_css_string(PrinterOptions::default())
                        && let Some(face) = parse_font_face(&text)
                    {
                        stylesheet.font_faces.push(face);
                    }
                }
                _ => {}
            }
        }

        stylesheet
    }

    pub fn extend(&mut self, other: Stylesheet) {
        let base = self.rules.len() as u32;
        self.rules.extend(other.rules.into_iter().map(|mut rule| {
            rule.order = rule.order.saturating_add(base);
            rule
        }));
        self.font_faces.extend(other.font_faces);
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn font_faces(&self) -> &[FontFace] {
        &self.font_faces
    }

    /// 作用于普通元素（不含伪元素）的声明，按层叠顺序排好。
    pub(crate) fn matched<'a>(&'a self, element: &NodeDataRef<ElementData>) -> Vec<Matched<'a>> {
        self.collect(element, None)
    }

    /// 作用于 `::before` / `::after` 的声明。
    pub(crate) fn matched_pseudo<'a>(
        &'a self,
        element: &NodeDataRef<ElementData>,
        pseudo: PseudoElement,
    ) -> Vec<Matched<'a>> {
        self.collect(element, Some(pseudo))
    }

    fn collect<'a>(
        &'a self,
        element: &NodeDataRef<ElementData>,
        pseudo: Option<PseudoElement>,
    ) -> Vec<Matched<'a>> {
        let mut output = Vec::new();
        for rule in &self.rules {
            let specificity = rule
                .selectors
                .iter()
                .filter(|selector| selector.pseudo == pseudo && selector.matcher.matches(element))
                .map(|selector| selector.specificity)
                .max();
            let Some(specificity) = specificity else {
                continue;
            };
            for declaration in &rule.declarations {
                output.push(Matched {
                    property: &declaration.property,
                    important: declaration.important,
                    specificity,
                    order: rule.order,
                });
            }
        }
        output.sort_by_key(|matched| (matched.important, matched.specificity, matched.order));
        output
    }
}

fn compile_selectors(
    selectors: &lightningcss::selector::SelectorList<'_>,
) -> Vec<CompiledSelector> {
    let mut output = Vec::new();
    for selector in &selectors.0 {
        let Ok(text) = selector.to_css_string(PrinterOptions::default()) else {
            continue;
        };
        let (base, pseudo) = split_pseudo(&text);
        if base.contains("::") || base.contains(":first-letter") || base.contains(":first-line") {
            continue;
        }
        let Ok(matcher) = Selectors::compile(&base) else {
            log::debug!("跳过不支持的选择器: {text}");
            continue;
        };
        let Some(compiled) = matcher.0.first() else {
            continue;
        };
        output.push(CompiledSelector {
            specificity: compiled.specificity(),
            matcher: Arc::new(matcher),
            pseudo,
        });
    }
    output
}

/// lightningcss 会把 `::before` 序列化成 `:before`，两种都认。
fn split_pseudo(text: &str) -> (String, Option<PseudoElement>) {
    let trimmed = text.trim_end();
    for (suffix, pseudo) in [
        ("::before", PseudoElement::Before),
        ("::after", PseudoElement::After),
        (":before", PseudoElement::Before),
        (":after", PseudoElement::After),
    ] {
        if let Some(base) = trimmed.strip_suffix(suffix) {
            return (base.trim_end().to_string(), Some(pseudo));
        }
    }
    (trimmed.to_string(), None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PseudoElement {
    Before,
    After,
}

/// 匹配时只需要元素、id 和 class 由 kuchiki/selectors 处理。
pub(crate) struct Matched<'a> {
    pub property: &'a Property,
    pub important: bool,
    specificity: Specificity,
    order: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Declaration {
    pub property: Property,
    pub important: bool,
}

// ---------------------------------------------------------------------------
// 声明 → 计算样式
// ---------------------------------------------------------------------------

/// 一条已解析的简化声明。简写属性在这里已经展开。
#[derive(Clone, Debug)]
pub(crate) enum Property {
    FontSize(Length),
    FontWeight(bool),
    FontStyle(bool),
    FontFamily(Vec<String>),
    Color(Color),
    Background(Color),
    Align(Align),
    LineHeight(LineHeight),
    Display(Display),
    MarginTop(Length),
    MarginRight(Length),
    MarginBottom(Length),
    MarginLeft(Length),
    PaddingTop(Length),
    PaddingRight(Length),
    PaddingBottom(Length),
    PaddingLeft(Length),
    BorderTop(BorderSpec),
    BorderRight(BorderSpec),
    BorderBottom(BorderSpec),
    BorderLeft(BorderSpec),
    Width(Length),
    Height(Length),
    Underline(bool),
    Strike(bool),
    Indent(Length),
    PreserveSpace(bool),
    Content(Content),
    BackgroundImage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Display {
    Inline,
    Block,
    None,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Length {
    Px(f32),
    Pt(f32),
    Em(f32),
    Rem(f32),
    Ex(f32),
    Percent(f32),
}

impl Length {
    pub(crate) fn resolve(self, base: f32, root: f32) -> f32 {
        match self {
            Length::Px(value) => value,
            Length::Pt(value) => value * 4.0 / 3.0,
            Length::Em(value) => value * base,
            Length::Rem(value) => value * root,
            Length::Ex(value) => value * base * 0.5,
            Length::Percent(value) => value * base / 100.0,
        }
    }

    pub(crate) fn fixed(self) -> Option<f32> {
        if matches!(self, Length::Percent(_)) {
            None
        } else {
            Some(self.resolve(ROOT_SIZE, ROOT_SIZE))
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LineHeight {
    Number(f32),
    Length(Length),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BorderSpec {
    pub width: f32,
    pub color: Option<Color>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Content {
    pub parts: Vec<ContentPart>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ContentPart {
    Text(String),
    Attr(String),
}

impl Content {
    /// 把 `content` 解析成实际文本；`attr()` 从元素属性取值。
    pub(crate) fn resolve(&self, attrs: &[(String, String)]) -> String {
        let mut output = String::new();
        for part in &self.parts {
            match part {
                ContentPart::Text(text) => output.push_str(text),
                ContentPart::Attr(name) => {
                    if let Some((_, value)) =
                        attrs.iter().find(|(key, _)| key.eq_ignore_ascii_case(name))
                    {
                        output.push_str(value);
                    }
                }
            }
        }
        output
    }
}

/// 元素的计算样式：继承项已从父元素复制，其余保持 `None`。
#[derive(Clone, Debug, Default)]
pub(crate) struct Computed {
    pub size: Option<f32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    pub color: Option<Color>,
    pub background: Option<Color>,
    pub font_family: Option<Vec<String>>,
    pub align: Option<Align>,
    pub line_height: Option<f32>,
    pub display: Option<Display>,
    pub margin_top: Option<f32>,
    pub margin_bottom: Option<f32>,
    pub margin_left: Option<f32>,
    pub margin_right: Option<f32>,
    pub padding: Edges,
    pub border_top: Option<Border>,
    pub border_right: Option<Border>,
    pub border_bottom: Option<Border>,
    pub border_left: Option<Border>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub indent: Option<f32>,
    pub preserve_space: bool,
    pub content: Option<Content>,
    /// 元素声明了背景图；行内文本用下划线近似（典型是词典的书名波浪线）。
    pub background_image: bool,
}

impl Computed {
    /// 只复制 CSS 中可继承的属性。
    pub(crate) fn inherited(parent: &Computed) -> Self {
        Self {
            size: parent.size,
            bold: parent.bold,
            italic: parent.italic,
            underline: parent.underline,
            strike: parent.strike,
            mono: parent.mono,
            color: parent.color,
            font_family: parent.font_family.clone(),
            align: parent.align,
            line_height: parent.line_height,
            preserve_space: parent.preserve_space,
            ..Default::default()
        }
    }

    pub(crate) fn apply(
        &mut self,
        matched: &[Matched<'_>],
        inline: &[Declaration],
        root_size: f32,
    ) {
        // 普通声明 → 行内普通声明 → !important 声明 → 行内 !important。
        for item in matched.iter().filter(|item| !item.important) {
            self.apply_property(item.property, root_size);
        }
        for declaration in inline.iter().filter(|item| !item.important) {
            self.apply_property(&declaration.property, root_size);
        }
        for item in matched.iter().filter(|item| item.important) {
            self.apply_property(item.property, root_size);
        }
        for declaration in inline.iter().filter(|item| item.important) {
            self.apply_property(&declaration.property, root_size);
        }
    }

    fn apply_property(&mut self, property: &Property, root_size: f32) {
        let font_size = self.size.unwrap_or(root_size);
        match property {
            Property::FontSize(length) => {
                self.size = Some(length.resolve(font_size, root_size).max(1.0));
            }
            Property::FontWeight(bold) => self.bold = *bold,
            Property::FontStyle(italic) => self.italic = *italic,
            Property::FontFamily(family) => self.font_family = Some(family.clone()),
            Property::Color(color) => self.color = Some(*color),
            Property::Background(color) => self.background = Some(*color),
            Property::Align(align) => self.align = Some(*align),
            Property::LineHeight(line_height) => {
                self.line_height = Some(match line_height {
                    LineHeight::Number(value) => *value,
                    LineHeight::Length(length) => {
                        let height = length.resolve(font_size, root_size);
                        if font_size > 0.0 {
                            height / font_size
                        } else {
                            1.4
                        }
                    }
                });
            }
            Property::Display(display) => self.display = Some(*display),
            Property::MarginTop(length) => {
                self.margin_top = Some(length.resolve(font_size, root_size));
            }
            Property::MarginRight(length) => {
                self.margin_right = Some(length.resolve(font_size, root_size));
            }
            Property::MarginBottom(length) => {
                self.margin_bottom = Some(length.resolve(font_size, root_size));
            }
            Property::MarginLeft(length) => {
                self.margin_left = Some(length.resolve(font_size, root_size));
            }
            Property::PaddingTop(length) => {
                self.padding.top = length.resolve(font_size, root_size);
            }
            Property::PaddingRight(length) => {
                self.padding.right = length.resolve(font_size, root_size);
            }
            Property::PaddingBottom(length) => {
                self.padding.bottom = length.resolve(font_size, root_size);
            }
            Property::PaddingLeft(length) => {
                self.padding.left = length.resolve(font_size, root_size);
            }
            Property::BorderTop(spec) => self.border_top = Some(border_from(*spec)),
            Property::BorderRight(spec) => self.border_right = Some(border_from(*spec)),
            Property::BorderBottom(spec) => self.border_bottom = Some(border_from(*spec)),
            Property::BorderLeft(spec) => self.border_left = Some(border_from(*spec)),
            Property::Width(length) => {
                self.width = (!matches!(length, Length::Percent(_)))
                    .then(|| length.resolve(font_size, root_size).max(0.0));
            }
            Property::Height(length) => {
                self.height = (!matches!(length, Length::Percent(_)))
                    .then(|| length.resolve(font_size, root_size).max(0.0));
            }
            Property::Underline(value) => self.underline = *value,
            Property::Strike(value) => self.strike = *value,
            Property::Indent(length) => {
                self.indent = Some(length.resolve(font_size, root_size));
            }
            Property::PreserveSpace(value) => self.preserve_space = *value,
            Property::Content(content) => self.content = Some(content.clone()),
            Property::BackgroundImage => self.background_image = true,
        }
    }
}

fn border_from(spec: BorderSpec) -> Border {
    Border {
        width: spec.width,
        color: spec.color,
    }
}

// ---------------------------------------------------------------------------
// lightningcss 声明 → 本模块的简化属性
// ---------------------------------------------------------------------------

fn convert_declarations(block: &DeclarationBlock<'_>) -> Vec<Declaration> {
    let mut output = Vec::new();
    for property in &block.declarations {
        push_serialized(&mut output, property, false);
    }
    for property in &block.important_declarations {
        push_serialized(&mut output, property, true);
    }
    output
}

fn push_serialized(
    output: &mut Vec<Declaration>,
    property: &lightningcss::properties::Property<'_>,
    important: bool,
) {
    let Ok(text) = property.to_css_string(false, PrinterOptions::default()) else {
        return;
    };
    let Some((name, value)) = text.split_once(':') else {
        return;
    };
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim();
    for property in parse_property(&name, value) {
        output.push(Declaration {
            property,
            important,
        });
    }
}

/// 解析行内 `style=""`。
pub(crate) fn parse_inline_style(style: &str) -> Vec<Declaration> {
    match DeclarationBlock::parse_string(
        style,
        ParserOptions {
            error_recovery: true,
            ..Default::default()
        },
    ) {
        Ok(block) => convert_declarations(&block),
        Err(error) => {
            log::debug!("行内样式解析失败: {error:?}");
            Vec::new()
        }
    }
}

fn parse_property(name: &str, value: &str) -> Vec<Property> {
    if value.is_empty() || value == "initial" || value == "inherit" || value == "unset" {
        return Vec::new();
    }
    match name {
        "color" => parse_color(value)
            .map(Property::Color)
            .into_iter()
            .collect(),
        "background-color" => parse_color(value)
            .map(Property::Background)
            .into_iter()
            .collect(),
        "background" => parse_background(value),
        "background-image" => {
            if value.contains("url(") {
                vec![Property::BackgroundImage]
            } else {
                Vec::new()
            }
        }
        "font-size" => parse_font_size(value)
            .map(Property::FontSize)
            .into_iter()
            .collect(),
        "font-weight" => parse_font_weight(value)
            .map(Property::FontWeight)
            .into_iter()
            .collect(),
        "font-style" => match value.to_ascii_lowercase().as_str() {
            "italic" | "oblique" => vec![Property::FontStyle(true)],
            "normal" => vec![Property::FontStyle(false)],
            _ => Vec::new(),
        },
        "font-family" => parse_family_list(value)
            .map(Property::FontFamily)
            .into_iter()
            .collect(),
        "font" => parse_font_shorthand(value),
        "text-align" => match value.to_ascii_lowercase().as_str() {
            "left" | "start" | "justify" => vec![Property::Align(Align::Left)],
            "center" => vec![Property::Align(Align::Center)],
            "right" | "end" => vec![Property::Align(Align::Right)],
            _ => Vec::new(),
        },
        "text-decoration" | "text-decoration-line" => {
            let value = value.to_ascii_lowercase();
            if value.contains("none") {
                vec![Property::Underline(false), Property::Strike(false)]
            } else {
                let underline = value.contains("underline");
                let strike = value.contains("line-through");
                if underline || strike {
                    vec![Property::Underline(underline), Property::Strike(strike)]
                } else {
                    Vec::new()
                }
            }
        }
        "line-height" => parse_line_height(value)
            .map(Property::LineHeight)
            .into_iter()
            .collect(),
        "display" => parse_display(value)
            .map(Property::Display)
            .into_iter()
            .collect(),
        "margin" => box_shorthand(value, BoxSide::Margin),
        "margin-top" => length_property(value, BoxSide::Margin, 0),
        "margin-right" => length_property(value, BoxSide::Margin, 1),
        "margin-bottom" => length_property(value, BoxSide::Margin, 2),
        "margin-left" => length_property(value, BoxSide::Margin, 3),
        "padding" => box_shorthand(value, BoxSide::Padding),
        "padding-top" => length_property(value, BoxSide::Padding, 0),
        "padding-right" => length_property(value, BoxSide::Padding, 1),
        "padding-bottom" => length_property(value, BoxSide::Padding, 2),
        "padding-left" => length_property(value, BoxSide::Padding, 3),
        "border" => border_shorthand(value, BorderSides::ALL),
        "border-top" => border_shorthand(value, BorderSides::TOP),
        "border-right" => border_shorthand(value, BorderSides::RIGHT),
        "border-bottom" => border_shorthand(value, BorderSides::BOTTOM),
        "border-left" => border_shorthand(value, BorderSides::LEFT),
        "width" => parse_length(value)
            .map(Property::Width)
            .into_iter()
            .collect(),
        "height" => parse_length(value)
            .map(Property::Height)
            .into_iter()
            .collect(),
        "text-indent" => parse_length(value)
            .map(Property::Indent)
            .into_iter()
            .collect(),
        "white-space" => match value.to_ascii_lowercase().as_str() {
            "pre" | "pre-wrap" | "pre-line" | "break-spaces" => {
                vec![Property::PreserveSpace(true)]
            }
            "normal" | "nowrap" => vec![Property::PreserveSpace(false)],
            _ => Vec::new(),
        },
        "content" => parse_content(value)
            .map(Property::Content)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_background(value: &str) -> Vec<Property> {
    let mut output = Vec::new();
    if let Some(color) = parse_color(value) {
        output.push(Property::Background(color));
    } else if let Some(color) = value.split_whitespace().find_map(parse_color) {
        output.push(Property::Background(color));
    }
    if value.contains("url(") {
        output.push(Property::BackgroundImage);
    }
    output
}

pub(crate) fn parse_family_list(value: &str) -> Option<Vec<String>> {
    let families: Vec<String> = split_top_level(value, ',')
        .into_iter()
        .map(|part| part.trim().trim_matches(['"', '\'']).trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    (!families.is_empty()).then_some(families)
}

fn parse_font_size(value: &str) -> Option<Length> {
    let lower = value.trim().to_ascii_lowercase();
    let keyword = match lower.as_str() {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "smaller" => Some(13.0),
        "larger" => Some(18.0),
        _ => None,
    };
    keyword.map(Length::Px).or_else(|| parse_length(&lower))
}

fn parse_font_weight(value: &str) -> Option<bool> {
    let lower = value.to_ascii_lowercase();
    match lower.as_str() {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        _ => lower.parse::<u32>().ok().map(|weight| weight >= 600),
    }
}

fn parse_font_shorthand(value: &str) -> Vec<Property> {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    let mut output = Vec::new();
    let mut size_index = None;
    for (index, token) in tokens.iter().enumerate() {
        let (size_token, line_height) = match token.split_once('/') {
            Some((size, height)) => (size, Some(height)),
            None => (*token, None),
        };
        if let Some(length) = parse_font_size(size_token) {
            output.push(Property::FontSize(length));
            if let Some(height) = line_height
                && let Some(line_height) = parse_line_height(height)
            {
                output.push(Property::LineHeight(line_height));
            }
            size_index = Some(index);
            break;
        }
    }
    let Some(size_index) = size_index else {
        return Vec::new();
    };
    for token in &tokens[..size_index] {
        match token.to_ascii_lowercase().as_str() {
            "bold" | "bolder" => output.push(Property::FontWeight(true)),
            "italic" | "oblique" => output.push(Property::FontStyle(true)),
            _ => {}
        }
    }
    let family = tokens[size_index + 1..].join(" ");
    if let Some(families) = parse_family_list(&family) {
        output.push(Property::FontFamily(families));
    }
    output
}

fn parse_line_height(value: &str) -> Option<LineHeight> {
    let value = value.trim();
    if let Ok(number) = value.parse::<f32>()
        && number.is_finite()
    {
        return Some(LineHeight::Number(number.max(0.5)));
    }
    parse_length(value).map(LineHeight::Length)
}

fn parse_display(value: &str) -> Option<Display> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Some(Display::None),
        // 没有真正的行内块布局，按行内处理才能保持文字流。
        "inline" | "inline-block" | "inline-flex" | "inline-grid" | "contents" => {
            Some(Display::Inline)
        }
        "block" | "flex" | "grid" | "list-item" | "table" | "table-row" | "table-cell"
        | "table-caption" => Some(Display::Block),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum BoxSide {
    Margin,
    Padding,
}

fn box_shorthand(value: &str, side: BoxSide) -> Vec<Property> {
    let mut lengths = Vec::new();
    for part in split_top_level(value, ' ') {
        if part.trim().is_empty() || part.trim() == "auto" {
            continue;
        }
        let Some(length) = parse_length(part.trim()) else {
            continue;
        };
        lengths.push(length);
    }
    if lengths.is_empty() || lengths.len() > 4 {
        return Vec::new();
    }
    let (top, right, bottom, left) = match lengths.len() {
        1 => (lengths[0], lengths[0], lengths[0], lengths[0]),
        2 => (lengths[0], lengths[1], lengths[0], lengths[1]),
        3 => (lengths[0], lengths[1], lengths[2], lengths[1]),
        _ => (lengths[0], lengths[1], lengths[2], lengths[3]),
    };
    let property = |index: usize, length: Length| match (side, index) {
        (BoxSide::Margin, 0) => Property::MarginTop(length),
        (BoxSide::Margin, 1) => Property::MarginRight(length),
        (BoxSide::Margin, 2) => Property::MarginBottom(length),
        (BoxSide::Margin, _) => Property::MarginLeft(length),
        (BoxSide::Padding, 0) => Property::PaddingTop(length),
        (BoxSide::Padding, 1) => Property::PaddingRight(length),
        (BoxSide::Padding, 2) => Property::PaddingBottom(length),
        (BoxSide::Padding, _) => Property::PaddingLeft(length),
    };
    vec![
        property(0, top),
        property(1, right),
        property(2, bottom),
        property(3, left),
    ]
}

fn length_property(value: &str, side: BoxSide, index: usize) -> Vec<Property> {
    let Some(length) = parse_length(value) else {
        return Vec::new();
    };
    let property = match (side, index) {
        (BoxSide::Margin, 0) => Property::MarginTop(length),
        (BoxSide::Margin, 1) => Property::MarginRight(length),
        (BoxSide::Margin, 2) => Property::MarginBottom(length),
        (BoxSide::Margin, _) => Property::MarginLeft(length),
        (BoxSide::Padding, 0) => Property::PaddingTop(length),
        (BoxSide::Padding, 1) => Property::PaddingRight(length),
        (BoxSide::Padding, 2) => Property::PaddingBottom(length),
        (BoxSide::Padding, _) => Property::PaddingLeft(length),
    };
    vec![property]
}

struct BorderSides(u8);

impl BorderSides {
    const TOP: Self = Self(1);
    const RIGHT: Self = Self(2);
    const BOTTOM: Self = Self(4);
    const LEFT: Self = Self(8);
    const ALL: Self = Self(15);
}

fn border_shorthand(value: &str, sides: BorderSides) -> Vec<Property> {
    let lower = value.to_ascii_lowercase();
    let mut width = 1.0f32;
    let mut color = None;
    let mut visible = true;
    for token in lower.split_whitespace() {
        if let Some(length) = parse_length(token) {
            width = length.resolve(ROOT_SIZE, ROOT_SIZE);
        } else if let Some(parsed) = parse_color(token) {
            color = Some(parsed);
        } else if token == "none" || token == "hidden" {
            visible = false;
        }
    }
    if !visible {
        width = 0.0;
    }
    let spec = BorderSpec { width, color };
    let mut output = Vec::new();
    if sides.0 & BorderSides::TOP.0 != 0 {
        output.push(Property::BorderTop(spec));
    }
    if sides.0 & BorderSides::RIGHT.0 != 0 {
        output.push(Property::BorderRight(spec));
    }
    if sides.0 & BorderSides::BOTTOM.0 != 0 {
        output.push(Property::BorderBottom(spec));
    }
    if sides.0 & BorderSides::LEFT.0 != 0 {
        output.push(Property::BorderLeft(spec));
    }
    output
}

pub(crate) fn parse_length(value: &str) -> Option<Length> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || value == "auto" || value == "initial" {
        return None;
    }
    if value == "0" {
        return Some(Length::Px(0.0));
    }
    let (number, unit) = split_number(&value)?;
    Some(match unit {
        "px" => Length::Px(number),
        "pt" => Length::Pt(number),
        "em" => Length::Em(number),
        "rem" => Length::Rem(number),
        "ex" => Length::Ex(number),
        "%" => Length::Percent(number),
        "pc" => Length::Pt(number * 12.0),
        "in" => Length::Px(number * 96.0),
        "cm" => Length::Px(number * 96.0 / 2.54),
        "mm" => Length::Px(number * 96.0 / 25.4),
        _ => return None,
    })
}

fn split_number(value: &str) -> Option<(f32, &str)> {
    let bytes = value.as_bytes();
    let mut end = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_digit() {
            seen_digit = true;
            end += 1;
        } else if byte == b'.' && !seen_dot {
            seen_dot = true;
            end += 1;
        } else if (byte == b'+' || byte == b'-') && end == 0 {
            end += 1;
        } else {
            break;
        }
    }
    if !seen_digit {
        return None;
    }
    let number: f32 = value[..end].parse().ok()?;
    Some((number, &value[end..]))
}

pub(crate) fn parse_color(value: &str) -> Option<Color> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    let lower = value.to_ascii_lowercase();
    if lower == "transparent" {
        return Some(Color::rgba(0, 0, 0, 0));
    }
    if lower.starts_with("rgb") {
        return parse_rgb(&lower);
    }
    named_color(&lower)
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let digits: Vec<u8> = hex
        .chars()
        .map(|ch| ch.to_digit(16).map(|value| value as u8))
        .collect::<Option<Vec<_>>>()?;
    match digits.len() {
        3 => Some(Color::rgb(digits[0] * 17, digits[1] * 17, digits[2] * 17)),
        4 => Some(Color::rgba(
            digits[0] * 17,
            digits[1] * 17,
            digits[2] * 17,
            digits[3] * 17,
        )),
        6 => Some(Color::rgb(
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        )),
        8 => Some(Color::rgba(
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
            digits[6] * 16 + digits[7],
        )),
        _ => None,
    }
}

fn parse_rgb(value: &str) -> Option<Color> {
    let open = value.find('(')?;
    let close = value.rfind(')')?;
    let parts: Vec<&str> = value[open + 1..close]
        .split([',', '/'])
        .flat_map(str::split_whitespace)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let channel = |part: &str| -> Option<u8> {
        if let Some(percent) = part.strip_suffix('%') {
            let value: f32 = percent.trim().parse().ok()?;
            Some((value * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8)
        } else {
            let value: f32 = part.trim().parse().ok()?;
            Some(value.round().clamp(0.0, 255.0) as u8)
        }
    };
    let alpha = match parts.get(3) {
        Some(part) => {
            if let Some(percent) = part.strip_suffix('%') {
                (percent.trim().parse::<f32>().ok()? * 2.55)
                    .round()
                    .clamp(0.0, 255.0) as u8
            } else {
                (part.trim().parse::<f32>().ok()? * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            }
        }
        None => 255,
    };
    Some(Color::rgba(
        channel(parts[0])?,
        channel(parts[1])?,
        channel(parts[2])?,
        alpha,
    ))
}

fn named_color(name: &str) -> Option<Color> {
    Some(match name {
        "black" => Color::rgb(0, 0, 0),
        "white" => Color::rgb(255, 255, 255),
        "red" => Color::rgb(255, 0, 0),
        "green" => Color::rgb(0, 128, 0),
        "lime" => Color::rgb(0, 255, 0),
        "blue" => Color::rgb(0, 0, 255),
        "yellow" => Color::rgb(255, 255, 0),
        "orange" => Color::rgb(255, 165, 0),
        "purple" => Color::rgb(128, 0, 128),
        "gray" | "grey" => Color::rgb(128, 128, 128),
        "silver" => Color::rgb(192, 192, 192),
        "maroon" => Color::rgb(128, 0, 0),
        "olive" => Color::rgb(128, 128, 0),
        "teal" => Color::rgb(0, 128, 128),
        "navy" => Color::rgb(0, 0, 128),
        "aqua" | "cyan" => Color::rgb(0, 255, 255),
        "fuchsia" | "magenta" => Color::rgb(255, 0, 255),
        "brown" => Color::rgb(165, 42, 42),
        "pink" => Color::rgb(255, 192, 203),
        "gold" => Color::rgb(255, 215, 0),
        "darkred" => Color::rgb(139, 0, 0),
        "darkblue" => Color::rgb(0, 0, 139),
        "darkgreen" => Color::rgb(0, 100, 0),
        "darkgray" | "darkgrey" => Color::rgb(169, 169, 169),
        "lightgray" | "lightgrey" => Color::rgb(211, 211, 211),
        "dimgray" | "dimgrey" => Color::rgb(105, 105, 105),
        "beige" => Color::rgb(245, 245, 220),
        "ivory" => Color::rgb(255, 255, 240),
        "darkolivegreen" => Color::rgb(85, 107, 47),
        "linen" => Color::rgb(250, 240, 230),
        "antiquewhite" => Color::rgb(250, 235, 215),
        "saddlebrown" => Color::rgb(139, 69, 19),
        "oldlace" => Color::rgb(253, 245, 230),
        "sienna" => Color::rgb(160, 82, 45),
        "currentcolor" => return None,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// content / @font-face
// ---------------------------------------------------------------------------

fn parse_content(value: &str) -> Option<Content> {
    let value = value.trim();
    if value.is_empty() || value == "none" || value == "normal" {
        return None;
    }
    let bytes = value.as_bytes();
    let mut parts = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            index += 1;
            let mut text = String::new();
            while index < bytes.len() && bytes[index] != quote {
                if bytes[index] == b'\\' && index + 1 < bytes.len() {
                    index += 1;
                    text.push(decode_css_escape(&mut index, value));
                } else {
                    let ch = value[index..].chars().next()?;
                    text.push(ch);
                    index += ch.len_utf8();
                }
            }
            index = (index + 1).min(bytes.len());
            parts.push(ContentPart::Text(text));
        } else if value[index..].starts_with("attr(") {
            let Some(end) = value[index + 5..].find(')') else {
                break;
            };
            let name = value[index + 5..index + 5 + end].trim().to_string();
            if !name.is_empty() {
                parts.push(ContentPart::Attr(name));
            }
            index += 5 + end + 1;
        } else {
            // counter() / url() / 其他关键字：跳过这个函数或单词。
            if let Some(end) = value[index..].find(|ch: char| ch.is_whitespace()) {
                index += end;
            } else {
                break;
            }
        }
    }
    Some(Content { parts })
}

fn decode_css_escape(index: &mut usize, value: &str) -> char {
    let bytes = value.as_bytes();
    let start = *index;
    let mut end = start;
    while end < bytes.len() && end < start + 6 && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    if end > start {
        let code = u32::from_str_radix(&value[start..end], 16).unwrap_or(0);
        *index = end;
        return char::from_u32(code).unwrap_or('\u{fffd}');
    }
    let ch = value[start..].chars().next().unwrap_or('\u{fffd}');
    *index += ch.len_utf8();
    ch
}

fn parse_font_face(text: &str) -> Option<FontFace> {
    let family = extract_css_property(text, "font-family")?;
    let family = parse_family_list(&family)?.into_iter().next()?;
    let sources = parse_urls(text);
    (!sources.is_empty()).then_some(FontFace { family, sources })
}

fn extract_css_property(text: &str, name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find(&format!("{name}:"))? + name.len() + 1;
    let rest = &text[start..];
    let end = rest.find(|ch| ch == ';' || ch == '}').unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

fn parse_urls(value: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut rest = value;
    while let Some(index) = rest.find("url(") {
        let after = &rest[index + 4..];
        let Some(end) = after.find(')') else {
            break;
        };
        let url = after[..end].trim().trim_matches(['"', '\'']);
        if !url.is_empty() {
            output.push(url.to_string());
        }
        rest = &after[end + 1..];
    }
    output
}

fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut output = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (index, ch) in text.char_indices() {
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            _ if depth == 0 && ch == separator => {
                output.push(&text[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    output.push(&text[start..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use kuchiki::NodeRef;
    use kuchiki::traits::TendrilSink;

    fn document(html: &str) -> (NodeRef, NodeDataRef<ElementData>) {
        let document = kuchiki::parse_html().one(html);
        let element = document
            .select_first(".target")
            .or_else(|_| document.select_first("body"))
            .expect("target element");
        (document, element)
    }

    fn computed_for(
        stylesheet: &Stylesheet,
        element: &NodeDataRef<ElementData>,
        pseudo: Option<PseudoElement>,
    ) -> Computed {
        let matched = match pseudo {
            Some(pseudo) => stylesheet.matched_pseudo(element, pseudo),
            None => stylesheet.matched(element),
        };
        let inline = element
            .attributes
            .borrow()
            .get("style")
            .map(parse_inline_style)
            .unwrap_or_default();
        let mut computed = Computed::default();
        computed.apply(&matched, &inline, ROOT_SIZE);
        computed
    }

    #[test]
    fn class_rules_apply_and_specificity_wins() {
        let sheet = Stylesheet::parse(
            ".target { color: #ff0000; font-size: 12px; }
             div.target { font-weight: bold; }
             .target { font-weight: normal; }",
        );
        let (_doc, element) = document(r#"<div class="target">x</div>"#);
        let style = computed_for(&sheet, &element, None);
        assert_eq!(style.color, Some(Color::rgb(255, 0, 0)));
        assert_eq!(style.size, Some(12.0));
        assert!(style.bold);
    }

    #[test]
    fn descendant_and_child_combinators() {
        let sheet =
            Stylesheet::parse(".target > .def { color: blue; } .target .phr { color: red; }");
        let document = kuchiki::parse_html().one(
            r#"<div class="target"><span class="def">a</span><span class="phr">b</span></div>"#,
        );
        let def = document.select_first(".def").unwrap();
        assert_eq!(
            computed_for(&sheet, &def, None).color,
            Some(Color::rgb(0, 0, 255))
        );
        let phr = document.select_first(".phr").unwrap();
        assert_eq!(
            computed_for(&sheet, &phr, None).color,
            Some(Color::rgb(255, 0, 0))
        );
    }

    #[test]
    fn attribute_selectors_and_pseudo_classes() {
        let sheet = Stylesheet::parse(
            r#"p[n] { color: red; }
               a[href$="WLK"] { color: green; }
               td:not(:empty) { font-weight: bold; }
               p:first-of-type { font-style: italic; }"#,
        );
        let document = kuchiki::parse_html().one(
            r#"<body><p n="❶">one</p><p>two</p>
               <a href="entry://WLK">link</a>
               <table><tr><td></td><td>x</td></tr></table></body>"#,
        );
        let paragraphs: Vec<_> = document.select("p").unwrap().collect();
        assert_eq!(paragraphs.len(), 2);
        let first = computed_for(&sheet, &paragraphs[0], None);
        assert_eq!(first.color, Some(Color::rgb(255, 0, 0)));
        assert!(first.italic);
        assert!(!computed_for(&sheet, &paragraphs[1], None).italic);

        let link = document.select_first("a").unwrap();
        assert_eq!(
            computed_for(&sheet, &link, None).color,
            Some(Color::rgb(0, 128, 0))
        );

        let cells: Vec<_> = document.select("td").unwrap().collect();
        assert!(!computed_for(&sheet, &cells[0], None).bold);
        assert!(computed_for(&sheet, &cells[1], None).bold);
    }

    #[test]
    fn sibling_combinators() {
        let sheet = Stylesheet::parse(".a + .b { color: red; } .a ~ .c { font-weight: bold; }");
        let document = kuchiki::parse_html().one(
            r#"<body><span class="a">a</span><span class="b">b</span><span class="c">c</span></body>"#,
        );
        let b = document.select_first(".b").unwrap();
        let c = document.select_first(".c").unwrap();
        assert_eq!(
            computed_for(&sheet, &b, None).color,
            Some(Color::rgb(255, 0, 0))
        );
        assert!(computed_for(&sheet, &c, None).bold);
    }

    #[test]
    fn pseudo_element_content_and_style() {
        let sheet = Stylesheet::parse(
            r#"w::before { content: "" attr(w) ""; color: #0a335c; font-size: 20px; }
               p[n]::before { content: attr(n); font-weight: bold; }"#,
        );
        let document = kuchiki::parse_html().one(r#"<body><p n="❶"><w w="禮"></w></p></body>"#);
        let w = document.select_first("w").unwrap();
        let style = computed_for(&sheet, &w, Some(PseudoElement::Before));
        let content = style.content.clone().unwrap();
        let attrs = vec![("w".to_string(), "禮".to_string())];
        assert_eq!(content.resolve(&attrs), "禮");
        assert_eq!(style.size, Some(20.0));
        assert_eq!(style.color, Some(Color::rgb(10, 51, 92)));

        let p = document.select_first("p").unwrap();
        let style = computed_for(&sheet, &p, Some(PseudoElement::Before));
        assert!(style.bold);
        assert_eq!(
            style
                .content
                .unwrap()
                .resolve(&[("n".to_string(), "❶".to_string())]),
            "❶"
        );
    }

    #[test]
    fn inline_style_and_important_order() {
        let sheet = Stylesheet::parse(".target { color: red !important; font-size: 10px; }");
        let (_doc, element) =
            document(r#"<div class="target" style="color: blue; font-size: 20px">x</div>"#);
        let style = computed_for(&sheet, &element, None);
        assert_eq!(style.color, Some(Color::rgb(255, 0, 0)));
        assert_eq!(style.size, Some(20.0));
    }

    #[test]
    fn inline_block_stays_inline() {
        let sheet = Stylesheet::parse(".target { display: inline-block; }");
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).display,
            Some(Display::Inline)
        );
    }

    #[test]
    fn margin_and_padding_shorthands_expand() {
        let sheet = Stylesheet::parse(".target { margin: 4px 8px; padding: 1px 2px 3px 4px; }");
        let (_doc, element) = document(r#"<div class="target">x</div>"#);
        let style = computed_for(&sheet, &element, None);
        assert_eq!(style.margin_top, Some(4.0));
        assert_eq!(style.margin_right, Some(8.0));
        assert_eq!(style.margin_bottom, Some(4.0));
        assert_eq!(style.margin_left, Some(8.0));
        assert_eq!(style.padding.top, 1.0);
        assert_eq!(style.padding.right, 2.0);
        assert_eq!(style.padding.bottom, 3.0);
        assert_eq!(style.padding.left, 4.0);
    }

    #[test]
    fn em_font_size_is_relative_to_parent() {
        let sheet = Stylesheet::parse(".target { font-size: 1.5em; }");
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        let mut parent = Computed::default();
        parent.size = Some(20.0);
        let matched = sheet.matched(&element);
        let mut style = Computed::inherited(&parent);
        style.apply(&matched, &[], ROOT_SIZE);
        assert_eq!(style.size, Some(30.0));
    }

    #[test]
    fn border_shorthand_parses_width_and_color() {
        let sheet = Stylesheet::parse(".target { border-bottom: 1px solid #888; }");
        let (_doc, element) = document(r#"<div class="target">x</div>"#);
        let style = computed_for(&sheet, &element, None);
        let border = style.border_bottom.expect("border");
        assert_eq!(border.width, 1.0);
        assert_eq!(border.color, Some(Color::rgb(136, 136, 136)));
    }

    #[test]
    fn font_face_and_urls_are_collected() {
        let sheet = Stylesheet::parse(
            r#"@font-face {
                 font-family: WL-special;
                 src: url("./WL-special.woff") format("woff");
               }"#,
        );
        let faces = sheet.font_faces();
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].family, "WL-special");
        assert_eq!(faces[0].sources, vec!["./WL-special.woff".to_string()]);
    }

    #[test]
    fn media_blocks_and_unknown_properties_are_skipped_safely() {
        let sheet = Stylesheet::parse(
            "@media (max-width: 20em) { .target { color: red; } }
             .target { letter-spacing: 1px; color: blue; }",
        );
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).color,
            Some(Color::rgb(0, 0, 255))
        );
    }

    #[test]
    fn content_parser_handles_concatenation() {
        let content = parse_content(r#""+" attr(rs) """#).unwrap();
        assert_eq!(
            content.resolve(&[("rs".to_string(), "13".to_string())]),
            "+13"
        );
        let content = parse_content(r#""引""#).unwrap();
        assert_eq!(content.resolve(&[]), "引");
        assert!(parse_content("none").is_none());
    }

    #[test]
    fn hex_and_named_colors() {
        assert_eq!(parse_color("#abc"), Some(Color::rgb(170, 187, 204)));
        assert_eq!(parse_color("rgb(10, 20, 30)"), Some(Color::rgb(10, 20, 30)));
        assert_eq!(
            parse_color("rgba(0,0,0,0.5)"),
            Some(Color::rgba(0, 0, 0, 128))
        );
        assert_eq!(parse_color("gray"), Some(Color::rgb(128, 128, 128)));
        assert_eq!(parse_color("teal"), Some(Color::rgb(0, 128, 128)));
    }
}
