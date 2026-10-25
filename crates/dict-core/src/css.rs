//! CSS 样式表的解析、层叠与计算样式。
//!
//! 语法解析交给 `lightningcss`，选择器匹配交给 `kuchiki`（Servo `selectors`），
//! 这里只保留 MDX 释义需要的属性子集、层叠顺序和继承逻辑。

use std::collections::HashMap;
use std::sync::Arc;

use kuchiki::{ElementData, NodeDataRef, Selectors, Specificity};
use lightningcss::declaration::DeclarationBlock;
use lightningcss::rules::CssRule;
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use lightningcss::traits::ToCss;

use crate::model::{
    Align, AlignItems, AlignSelf, Border, Color, Edges, FlexDirection, FlexWrap, FloatSide, Justify,
};

/// CSS 的初始字号；用于 `rem` 以及没有 css 字号时的 `em`。
pub(crate) const ROOT_SIZE: f32 = 16.0;

/// 词典窗口的内容尺寸，用于求值 `@media` 与 `vw/vh`。
const VIEWPORT_WIDTH: f32 = 420.0;
const VIEWPORT_HEIGHT: f32 = 800.0;

/// 一次样式表解析的覆盖率统计，用来定位“哪类规则还没支持”。
#[derive(Clone, Debug, Default)]
pub struct CssStats {
    pub style_rules: usize,
    pub compiled_rules: usize,
    /// `@media` / `@supports` / `@layer` / `@import` 等出现次数。
    pub at_rules: HashMap<String, usize>,
    /// 无法映射到内部属性的声明名及次数。
    pub unsupported_declarations: HashMap<String, usize>,
    pub unsupported_selectors: usize,
}

impl CssStats {
    fn merge(&mut self, other: &CssStats) {
        self.style_rules += other.style_rules;
        self.compiled_rules += other.compiled_rules;
        self.unsupported_selectors += other.unsupported_selectors;
        for (name, count) in &other.at_rules {
            *self.at_rules.entry(name.clone()).or_default() += count;
        }
        for (name, count) in &other.unsupported_declarations {
            *self
                .unsupported_declarations
                .entry(name.clone())
                .or_default() += count;
        }
    }

    pub fn unsupported_declarations_total(&self) -> usize {
        self.unsupported_declarations.values().sum()
    }
}

/// 一份样式表；`extend` 用于把词条内嵌样式接在全局样式后面。
#[derive(Clone, Default)]
pub struct Stylesheet {
    rules: Vec<CompiledRule>,
    font_faces: Vec<FontFace>,
    stats: CssStats,
    normal_rules: Vec<usize>,
    pseudo_before_rules: Vec<usize>,
    pseudo_after_rules: Vec<usize>,
}

#[derive(Clone)]
struct CompiledRule {
    selectors: Vec<CompiledSelector>,
    declarations: Vec<Declaration>,
    order: u32,
    media: Option<MediaCondition>,
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

        let mut order = 0;
        collect_rules(
            &parsed.rules.0,
            None,
            &mut order,
            &mut stylesheet.rules,
            &mut stylesheet.font_faces,
            &mut stylesheet.stats,
        );
        stylesheet.rebuild_indices();
        stylesheet
    }

    pub fn extend(&mut self, other: Stylesheet) {
        let base = self.rules.len() as u32;
        self.rules.extend(other.rules.into_iter().map(|mut rule| {
            rule.order = rule.order.saturating_add(base);
            rule
        }));
        self.font_faces.extend(other.font_faces);
        self.stats.merge(&other.stats);
        self.rebuild_indices();
    }

    fn rebuild_indices(&mut self) {
        self.normal_rules.clear();
        self.pseudo_before_rules.clear();
        self.pseudo_after_rules.clear();

        for (idx, rule) in self.rules.iter().enumerate() {
            let mut has_normal = false;
            let mut has_before = false;
            let mut has_after = false;
            for selector in &rule.selectors {
                match selector.pseudo {
                    None => has_normal = true,
                    Some(PseudoElement::Before) => has_before = true,
                    Some(PseudoElement::After) => has_after = true,
                }
            }
            if has_normal {
                self.normal_rules.push(idx);
            }
            if has_before {
                self.pseudo_before_rules.push(idx);
            }
            if has_after {
                self.pseudo_after_rules.push(idx);
            }
        }
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

    pub fn stats(&self) -> &CssStats {
        &self.stats
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
        let rule_indices = match pseudo {
            None => &self.normal_rules,
            Some(PseudoElement::Before) => &self.pseudo_before_rules,
            Some(PseudoElement::After) => &self.pseudo_after_rules,
        };
        if rule_indices.is_empty() {
            return Vec::new();
        }

        let mut output = Vec::new();
        for &idx in rule_indices {
            let rule = &self.rules[idx];
            if rule
                .media
                .as_ref()
                .is_some_and(|media| !media.matches(VIEWPORT_WIDTH, VIEWPORT_HEIGHT))
            {
                continue;
            }
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
                    declaration,
                    important: declaration.important,
                    specificity,
                    order: rule.order,
                });
            }
        }
        if output.len() > 1 {
            output.sort_by_key(|matched| (matched.important, matched.specificity, matched.order));
        }
        output
    }
}

fn collect_rules<'i, R>(
    rules: &[CssRule<'i, R>],
    media: Option<MediaCondition>,
    order: &mut u32,
    output: &mut Vec<CompiledRule>,
    font_faces: &mut Vec<FontFace>,
    stats: &mut CssStats,
) {
    fn walk<'i, R>(
        rules: &[CssRule<'i, R>],
        media: Option<MediaCondition>,
        order: &mut u32,
        output: &mut Vec<CompiledRule>,
        font_faces: &mut Vec<FontFace>,
        stats: &mut CssStats,
    ) {
        for rule in rules {
            match rule {
                CssRule::Style(style) => {
                    stats.style_rules += 1;
                    let selectors = compile_selectors(&style.selectors, stats);
                    if selectors.is_empty() {
                        continue;
                    }
                    let declarations = convert_declarations(&style.declarations, stats);
                    if declarations.is_empty() {
                        continue;
                    }
                    stats.compiled_rules += 1;
                    output.push(CompiledRule {
                        selectors,
                        declarations,
                        order: *order,
                        media: media.clone(),
                    });
                    *order = order.saturating_add(1);
                }
                CssRule::Media(rule) => {
                    *stats.at_rules.entry("media".into()).or_default() += 1;
                    let query = rule
                        .query
                        .to_css_string(PrinterOptions::default())
                        .unwrap_or_default();
                    let condition = parse_media_condition(&query);
                    let combined = match media.clone() {
                        Some(parent) => Some(parent.and(condition)),
                        None => Some(condition),
                    };
                    walk(&rule.rules.0, combined, order, output, font_faces, stats);
                }
                CssRule::Supports(rule) => {
                    *stats.at_rules.entry("supports".into()).or_default() += 1;
                    let condition = rule
                        .condition
                        .to_css_string(PrinterOptions::default())
                        .unwrap_or_default();
                    if supports_condition_matches(&condition) {
                        walk(
                            &rule.rules.0,
                            media.clone(),
                            order,
                            output,
                            font_faces,
                            stats,
                        );
                    }
                }
                CssRule::LayerBlock(rule) => {
                    *stats.at_rules.entry("layer".into()).or_default() += 1;
                    walk(
                        &rule.rules.0,
                        media.clone(),
                        order,
                        output,
                        font_faces,
                        stats,
                    );
                }
                CssRule::Import(_) => {
                    *stats.at_rules.entry("import".into()).or_default() += 1;
                }
                CssRule::FontFace(face) => {
                    if let Ok(text) = face.to_css_string(PrinterOptions::default())
                        && let Some(face) = parse_font_face(&text)
                    {
                        font_faces.push(face);
                    }
                }
                CssRule::Keyframes(_) => {
                    *stats.at_rules.entry("keyframes".into()).or_default() += 1;
                }
                CssRule::LayerStatement(_) => {
                    *stats.at_rules.entry("layer".into()).or_default() += 1;
                }
                CssRule::Nesting(rule) => {
                    *stats.at_rules.entry("nesting".into()).or_default() += 1;
                    stats.style_rules += 1;
                    let selectors = compile_selectors(&rule.style.selectors, stats);
                    if !selectors.is_empty() {
                        let declarations = convert_declarations(&rule.style.declarations, stats);
                        if !declarations.is_empty() {
                            stats.compiled_rules += 1;
                            output.push(CompiledRule {
                                selectors,
                                declarations,
                                order: *order,
                                media: media.clone(),
                            });
                            *order = order.saturating_add(1);
                        }
                    }
                }
                CssRule::Property(_) => {
                    *stats.at_rules.entry("property".into()).or_default() += 1;
                }
                CssRule::Page(_) => {
                    *stats.at_rules.entry("page".into()).or_default() += 1;
                }
                CssRule::Namespace(_) => {
                    *stats.at_rules.entry("namespace".into()).or_default() += 1;
                }
                CssRule::CustomMedia(_) => {
                    *stats.at_rules.entry("custom-media".into()).or_default() += 1;
                }
                CssRule::Scope(rule) => {
                    *stats.at_rules.entry("scope".into()).or_default() += 1;
                    walk(
                        &rule.rules.0,
                        media.clone(),
                        order,
                        output,
                        font_faces,
                        stats,
                    );
                }
                CssRule::StartingStyle(rule) => {
                    *stats.at_rules.entry("starting-style".into()).or_default() += 1;
                    walk(
                        &rule.rules.0,
                        media.clone(),
                        order,
                        output,
                        font_faces,
                        stats,
                    );
                }
                CssRule::ViewTransition(_) => {
                    *stats.at_rules.entry("view-transition".into()).or_default() += 1;
                }
                CssRule::Unknown(rule) => {
                    *stats.at_rules.entry(format!("@{}", rule.name)).or_default() += 1;
                }
                _ => {
                    *stats.at_rules.entry("other".into()).or_default() += 1;
                }
            }
        }
    }

    walk(rules, media, order, output, font_faces, stats);
}

// ---------------------------------------------------------------------------
// @media / @supports
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct MediaCondition {
    queries: Vec<MediaQuery>,
}

#[derive(Clone, Debug, Default)]
struct MediaQuery {
    features: Vec<MediaFeature>,
    negated: bool,
    type_ok: bool,
}

#[derive(Clone, Debug)]
struct MediaFeature {
    name: String,
    value: String,
}

impl MediaCondition {
    fn matches(&self, width: f32, height: f32) -> bool {
        if self.queries.is_empty() {
            return true;
        }
        self.queries
            .iter()
            .any(|query| query.matches(width, height))
    }

    fn and(self, other: MediaCondition) -> MediaCondition {
        if self.queries.is_empty() {
            return other;
        }
        if other.queries.is_empty() {
            return self;
        }
        let mut queries = Vec::new();
        for left in &self.queries {
            for right in &other.queries {
                let mut merged = left.clone();
                merged.features.extend(right.features.clone());
                merged.negated |= right.negated;
                merged.type_ok &= right.type_ok;
                queries.push(merged);
            }
        }
        MediaCondition { queries }
    }
}

impl MediaQuery {
    fn matches(&self, width: f32, height: f32) -> bool {
        let matched = self.type_ok
            && self
                .features
                .iter()
                .all(|feature| feature.matches(width, height));
        matched != self.negated
    }
}

impl MediaFeature {
    fn matches(&self, width: f32, height: f32) -> bool {
        let value = self.value.trim();
        match self.name.trim() {
            "min-width" => length_px(value).is_some_and(|value| width >= value),
            "max-width" => length_px(value).is_some_and(|value| width <= value),
            "width" => length_px(value).is_some_and(|value| (width - value).abs() < 0.5),
            "min-height" => length_px(value).is_some_and(|value| height >= value),
            "max-height" => length_px(value).is_some_and(|value| height <= value),
            "height" => length_px(value).is_some_and(|value| (height - value).abs() < 0.5),
            "orientation" => match value {
                "landscape" => width >= height,
                "portrait" => width < height,
                _ => true,
            },
            "prefers-color-scheme" => value != "dark",
            "prefers-reduced-motion" | "prefers-contrast" | "forced-colors" => true,
            // 分辨率、hover、指针类型等对静态排版影响小；不因为不认识就丢掉规则。
            _ => true,
        }
    }
}

fn parse_media_condition(text: &str) -> MediaCondition {
    let text = text.trim();
    if text.is_empty() {
        return MediaCondition::default();
    }

    let mut queries = Vec::new();
    for part in split_top_level(text, ',') {
        let mut query_text = part.trim().to_ascii_lowercase();
        if query_text.is_empty() {
            continue;
        }

        let mut negated = false;
        if let Some(rest) = query_text.strip_prefix("not ") {
            negated = true;
            query_text = rest.trim().to_string();
        }
        if let Some(rest) = query_text.strip_prefix("only ") {
            query_text = rest.trim().to_string();
        }

        let media_type = query_text
            .split(|ch: char| ch.is_whitespace() || ch == '(')
            .next()
            .unwrap_or_default();
        let type_ok = !matches!(
            media_type,
            "print"
                | "speech"
                | "aural"
                | "braille"
                | "embossed"
                | "handheld"
                | "projection"
                | "tty"
                | "tv"
        );

        let mut features = Vec::new();
        let mut rest = query_text.as_str();
        while let Some(start) = rest.find('(') {
            let after = &rest[start + 1..];
            let Some(end) = after.find(')') else {
                break;
            };
            let expression = &after[..end];
            if let Some((name, value)) = expression.split_once(':') {
                features.push(MediaFeature {
                    name: name.trim().to_string(),
                    value: value.trim().to_string(),
                });
            } else {
                features.extend(parse_media_range(expression));
            }
            rest = &after[end + 1..];
        }

        queries.push(MediaQuery {
            features,
            negated,
            type_ok,
        });
    }

    MediaCondition { queries }
}

fn parse_media_range(expression: &str) -> Vec<MediaFeature> {
    let Some((left, op, right)) = split_comparison(expression) else {
        return Vec::new();
    };
    let left = left.trim();
    let right = right.trim();

    if parse_length(left).is_some() {
        // `400px <= width` 或 `400px <= width <= 1000px`
        if let Some((name, next_op, value)) = split_comparison(right) {
            let mut features = media_feature_for(name.trim(), &invert_comparison(op), left);
            features.extend(media_feature_for(name.trim(), next_op, value.trim()));
            return features;
        }
        return media_feature_for(right, &invert_comparison(op), left);
    }

    if parse_length(right).is_some() {
        return media_feature_for(left, op, right);
    }

    Vec::new()
}

fn media_feature_for(name: &str, op: &str, value: &str) -> Vec<MediaFeature> {
    let name = name.trim().to_ascii_lowercase();
    if !matches!(name.as_str(), "width" | "height") {
        return Vec::new();
    }
    let value = value.trim().to_string();
    match op {
        ">=" | ">" => vec![MediaFeature {
            name: format!("min-{name}"),
            value,
        }],
        "<=" | "<" => vec![MediaFeature {
            name: format!("max-{name}"),
            value,
        }],
        "=" => vec![MediaFeature { name, value }],
        _ => Vec::new(),
    }
}

fn invert_comparison(op: &str) -> String {
    match op {
        ">=" => "<=".to_string(),
        "<=" => ">=".to_string(),
        ">" => "<".to_string(),
        "<" => ">".to_string(),
        _ => op.to_string(),
    }
}

fn split_comparison(value: &str) -> Option<(&str, &str, &str)> {
    for operator in [">=", "<=", ">", "<", "="] {
        if let Some(index) = value.find(operator) {
            return Some((&value[..index], operator, &value[index + operator.len()..]));
        }
    }
    None
}

fn length_px(value: &str) -> Option<f32> {
    let value = value.trim();
    parse_length(value).and_then(|length| length.fixed())
}

fn supports_condition_matches(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return true;
    }
    if let Some(rest) = strip_outer_parens(text) {
        let inner = rest.trim();
        if let Some(rest) = inner.strip_prefix("not ") {
            return !supports_condition_matches(rest);
        }
        if let Some((name, value)) = inner.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            if name.starts_with("--") {
                return true;
            }
            return !parse_property(&name, value.trim()).is_empty();
        }
        return supports_condition_matches(inner);
    }
    if let Some(parts) = split_top_level_keyword(text, " or ") {
        return parts.into_iter().any(supports_condition_matches);
    }
    if let Some(parts) = split_top_level_keyword(text, " and ") {
        return parts.into_iter().all(supports_condition_matches);
    }
    if let Some(rest) = text.strip_prefix("not ") {
        return !supports_condition_matches(rest);
    }
    if let Some(rest) = text
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    {
        let Some((name, value)) = rest.split_once(':') else {
            return false;
        };
        let name = name.trim().to_ascii_lowercase();
        if name.starts_with("--") {
            return true;
        }
        return !parse_property(&name, value.trim()).is_empty();
    }
    false
}

fn strip_outer_parens(text: &str) -> Option<&str> {
    let text = text.trim();
    let inner = text.strip_prefix('(')?.strip_suffix(')')?;
    let mut depth = 0i32;
    for ch in inner.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    (depth == 0).then_some(inner)
}

fn split_top_level_keyword<'a>(text: &'a str, keyword: &str) -> Option<Vec<&'a str>> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    let mut index = 0;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        let ch = text[index..].chars().next()?;
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            index += ch.len_utf8();
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if depth == 0 && text[index..].starts_with(keyword) => {
                parts.push(text[start..index].trim());
                index += keyword.len();
                start = index;
                continue;
            }
            _ => {}
        }
        index += ch.len_utf8();
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(text[start..].trim());
    Some(parts)
}

fn compile_selectors(
    selectors: &lightningcss::selector::SelectorList<'_>,
    stats: &mut CssStats,
) -> Vec<CompiledSelector> {
    let mut output = Vec::new();
    for selector in &selectors.0 {
        let Ok(text) = selector.to_css_string(PrinterOptions::default()) else {
            stats.unsupported_selectors += 1;
            continue;
        };
        let (base, pseudo) = split_pseudo(&text);
        if base.contains("::") || base.contains(":first-letter") || base.contains(":first-line") {
            stats.unsupported_selectors += 1;
            continue;
        }
        let Ok(matcher) = Selectors::compile(&base) else {
            stats.unsupported_selectors += 1;
            log::debug!("跳过不支持的选择器: {text}");
            continue;
        };
        let Some(compiled) = matcher.0.first() else {
            stats.unsupported_selectors += 1;
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
    pub declaration: &'a Declaration,
    pub important: bool,
    specificity: Specificity,
    order: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
    /// 不含 `var()` / `calc()` 时预先解析好的属性；动态值留空，apply 时再解析。
    pub properties: Vec<Property>,
}

impl Declaration {
    fn is_dynamic(&self) -> bool {
        has_dynamic_value(&self.value)
    }

    fn is_custom(&self) -> bool {
        self.name.starts_with("--")
    }
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
    Visibility(bool),
    Float(FloatSide),
    VerticalAlign(AlignSelf),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    Justify(Justify),
    AlignItems(AlignItems),
    AlignSelf(AlignSelf),
    /// `gap` / `row-gap` / `column-gap`：只保留一个值，见 [`Computed::gap`]。
    Gap(Length),
    FlexGrow(f32),
    FlexShrink(f32),
    FlexBasis(Length),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Display {
    Inline,
    InlineBlock,
    Block,
    Flex,
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
    Vw(f32),
    Vh(f32),
    Vmin(f32),
    Vmax(f32),
    Ch(f32),
    Lh(f32),
    /// `calc()` / `min()` / `max()` / `clamp()` 的结果：绝对部分 + 百分比部分。
    Calc {
        px: f32,
        percent: f32,
    },
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
            Length::Vw(value) => value * VIEWPORT_WIDTH / 100.0,
            Length::Vh(value) => value * VIEWPORT_HEIGHT / 100.0,
            Length::Vmin(value) => value * VIEWPORT_WIDTH.min(VIEWPORT_HEIGHT) / 100.0,
            Length::Vmax(value) => value * VIEWPORT_WIDTH.max(VIEWPORT_HEIGHT) / 100.0,
            Length::Ch(value) => value * base * 0.5,
            Length::Lh(value) => value * base * 1.2,
            Length::Calc { px, percent } => px + percent * base / 100.0,
        }
    }

    pub(crate) fn fixed(self) -> Option<f32> {
        if self.has_percent() {
            None
        } else {
            Some(self.resolve(ROOT_SIZE, ROOT_SIZE))
        }
    }

    pub(crate) fn has_percent(self) -> bool {
        match self {
            Length::Percent(_) => true,
            Length::Calc { percent, .. } => percent.abs() > f32::EPSILON,
            _ => false,
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
    // ---- flex 排版 ----
    pub flex_direction: Option<FlexDirection>,
    pub flex_wrap: Option<FlexWrap>,
    pub justify: Option<Justify>,
    pub align_items: Option<AlignItems>,
    pub align_self: Option<AlignSelf>,
    /// 主轴/交叉轴间距。词典里的双栏对照几乎都写 `gap: Npx`，
    /// 分轴写法（`row-gap` / `column-gap`）只保留最后一次声明，够用且省一个字段。
    pub gap: Option<f32>,
    pub flex_grow: Option<f32>,
    pub flex_shrink: Option<f32>,
    pub flex_basis: Option<f32>,
    /// CSS 是否显式声明过 `text-decoration`（包括 `none`）。
    /// 界面据此决定要不要给链接补默认下划线。
    pub decoration_explicit: bool,
    /// CSS 自定义属性（`--x`），按规范参与继承。
    pub custom: HashMap<String, String>,
    /// `visibility: hidden`（占位但仍隐藏；这里直接不生成内容）。
    pub hidden: bool,
    /// `float`；只在块级布局里做左右对齐近似。
    pub float: Option<FloatSide>,
    /// `vertical-align`，用于 inline-block 的交叉轴对齐。
    pub vertical_align: Option<AlignSelf>,
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
            custom: parent.custom.clone(),
            hidden: parent.hidden,
            ..Default::default()
        }
    }

    pub(crate) fn apply(
        &mut self,
        matched: &[Matched<'_>],
        inline: &[Declaration],
        root_size: f32,
    ) {
        // `--x` 先按层叠顺序结算，后面的声明才能用 `var()` 引用。
        self.apply_custom(
            matched
                .iter()
                .filter(|item| !item.important)
                .map(|item| item.declaration),
        );
        self.apply_custom(inline.iter().filter(|item| !item.important));
        self.apply_custom(
            matched
                .iter()
                .filter(|item| item.important)
                .map(|item| item.declaration),
        );
        self.apply_custom(inline.iter().filter(|item| item.important));

        // 普通声明 → 行内普通声明 → !important 声明 → 行内 !important。
        self.apply_declarations(
            matched
                .iter()
                .filter(|item| !item.important)
                .map(|item| item.declaration),
            root_size,
        );
        self.apply_declarations(inline.iter().filter(|item| !item.important), root_size);
        self.apply_declarations(
            matched
                .iter()
                .filter(|item| item.important)
                .map(|item| item.declaration),
            root_size,
        );
        self.apply_declarations(inline.iter().filter(|item| item.important), root_size);
    }

    fn apply_custom<'a>(&mut self, declarations: impl Iterator<Item = &'a Declaration>) {
        for declaration in declarations {
            if !declaration.is_custom() {
                continue;
            }
            if let Some(value) = substitute_vars(&declaration.value, &self.custom, 0) {
                self.custom.insert(declaration.name.clone(), value);
            }
        }
    }

    fn apply_declarations<'a>(
        &mut self,
        declarations: impl Iterator<Item = &'a Declaration>,
        root_size: f32,
    ) {
        for declaration in declarations {
            if declaration.is_custom() {
                continue;
            }
            if declaration.is_dynamic() {
                let Some(value) = substitute_vars(&declaration.value, &self.custom, 0) else {
                    continue;
                };
                let value = replace_current_color(&value, self.color);
                let properties = parse_property(&declaration.name, &value);
                for property in &properties {
                    self.apply_property(property, root_size);
                }
            } else {
                for property in &declaration.properties {
                    self.apply_property(property, root_size);
                }
            }
        }
    }
}

fn substitute_vars(value: &str, custom: &HashMap<String, String>, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    let Some(first) = lower.find("var(") else {
        return Some(value.to_string());
    };

    let mut output = String::with_capacity(value.len());
    output.push_str(&value[..first]);
    let mut rest_index = first;

    loop {
        let lower = value.to_ascii_lowercase();
        let Some(start) = lower[rest_index..].find("var(") else {
            output.push_str(&value[rest_index..]);
            break;
        };
        let start = rest_index + start;
        output.push_str(&value[rest_index..start]);
        let inner_start = start + 4;
        let Some(end) = find_matching_paren(value, inner_start) else {
            return None;
        };
        let inner = &value[inner_start..end];
        let parts = split_top_level(inner, ',');
        let name = parts.first()?.trim();
        let fallback = parts.get(1).map(|value| value.trim());

        let resolved = custom
            .get(name)
            .and_then(|value| substitute_vars(value, custom, depth + 1))
            .or_else(|| fallback.and_then(|value| substitute_vars(value, custom, depth + 1)))?;
        output.push_str(&resolved);
        rest_index = end + 1;
    }

    Some(output)
}

fn find_matching_paren(value: &str, start: usize) -> Option<usize> {
    let mut depth = 1i32;
    let mut quote: Option<char> = None;
    for (offset, ch) in value[start..].char_indices() {
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn replace_current_color(value: &str, color: Option<Color>) -> String {
    let lower = value.to_ascii_lowercase();
    if !lower.contains("currentcolor") {
        return value.to_string();
    }
    let color = color.unwrap_or(Color::rgb(0, 0, 0));
    let replacement = format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);

    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.to_ascii_lowercase().find("currentcolor") {
        output.push_str(&rest[..index]);
        output.push_str(&replacement);
        rest = &rest[index + "currentcolor".len()..];
    }
    output.push_str(rest);
    output
}

impl Computed {
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
            Property::BorderTop(spec) => self.border_top = Some(border_from(*spec, self.color)),
            Property::BorderRight(spec) => self.border_right = Some(border_from(*spec, self.color)),
            Property::BorderBottom(spec) => {
                self.border_bottom = Some(border_from(*spec, self.color))
            }
            Property::BorderLeft(spec) => self.border_left = Some(border_from(*spec, self.color)),
            Property::Width(length) => {
                self.width =
                    (!length.has_percent()).then(|| length.resolve(font_size, root_size).max(0.0));
            }
            Property::Height(length) => {
                self.height =
                    (!length.has_percent()).then(|| length.resolve(font_size, root_size).max(0.0));
            }
            Property::Underline(value) => {
                self.underline = *value;
                self.decoration_explicit = true;
            }
            Property::Strike(value) => self.strike = *value,
            Property::Indent(length) => {
                self.indent = Some(length.resolve(font_size, root_size));
            }
            Property::PreserveSpace(value) => self.preserve_space = *value,
            Property::Content(content) => self.content = Some(content.clone()),
            Property::Visibility(visible) => self.hidden = !visible,
            Property::Float(side) => self.float = Some(*side),
            Property::VerticalAlign(align) => self.vertical_align = Some(*align),
            Property::BackgroundImage => self.background_image = true,
            Property::FlexDirection(direction) => self.flex_direction = Some(*direction),
            Property::FlexWrap(wrap) => self.flex_wrap = Some(*wrap),
            Property::Justify(justify) => self.justify = Some(*justify),
            Property::AlignItems(align) => self.align_items = Some(*align),
            Property::AlignSelf(align) => self.align_self = Some(*align),
            Property::Gap(length) => {
                self.gap = Some(length.resolve(font_size, root_size).max(0.0));
            }
            Property::FlexGrow(grow) => self.flex_grow = Some(grow.max(0.0)),
            Property::FlexShrink(shrink) => self.flex_shrink = Some(shrink.max(0.0)),
            Property::FlexBasis(length) => {
                // 百分比基准要等布局宽度，这里退化成 `auto`。
                self.flex_basis =
                    (!length.has_percent()).then(|| length.resolve(font_size, root_size).max(0.0));
            }
        }
    }
}

fn border_from(spec: BorderSpec, current_color: Option<Color>) -> Border {
    Border {
        width: spec.width,
        color: spec.color.or(current_color),
    }
}

// ---------------------------------------------------------------------------
// lightningcss 声明 → 本模块的简化属性
// ---------------------------------------------------------------------------

fn convert_declarations(block: &DeclarationBlock<'_>, stats: &mut CssStats) -> Vec<Declaration> {
    let mut output = Vec::new();
    for property in &block.declarations {
        push_serialized(&mut output, property, false, stats);
    }
    for property in &block.important_declarations {
        push_serialized(&mut output, property, true, stats);
    }
    output
}

fn push_serialized(
    output: &mut Vec<Declaration>,
    property: &lightningcss::properties::Property<'_>,
    important: bool,
    stats: &mut CssStats,
) {
    let Ok(text) = property.to_css_string(false, PrinterOptions::default()) else {
        return;
    };
    let Some((name, value)) = text.split_once(':') else {
        return;
    };
    let name = name.trim().to_ascii_lowercase();
    let value = value.trim();
    let custom = name.starts_with("--");
    let dynamic = has_dynamic_value(value);
    let properties = if dynamic || custom {
        Vec::new()
    } else {
        parse_property(&name, value)
    };

    if !custom && !dynamic && properties.is_empty() {
        *stats
            .unsupported_declarations
            .entry(name.clone())
            .or_default() += 1;
        return;
    }

    output.push(Declaration {
        name,
        value: value.to_string(),
        important,
        properties,
    });
}

fn has_dynamic_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    ["var(", "calc(", "min(", "max(", "clamp(", "currentcolor"]
        .iter()
        .any(|needle| lower.contains(needle))
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
        Ok(block) => {
            let mut stats = CssStats::default();
            convert_declarations(&block, &mut stats)
        }
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
        "visibility" => match value.to_ascii_lowercase().as_str() {
            "hidden" | "collapse" => vec![Property::Visibility(false)],
            "visible" => vec![Property::Visibility(true)],
            _ => Vec::new(),
        },
        "float" => match value.to_ascii_lowercase().as_str() {
            "left" => vec![Property::Float(FloatSide::Left)],
            "right" => vec![Property::Float(FloatSide::Right)],
            _ => Vec::new(),
        },
        "vertical-align" => parse_vertical_align(value)
            .map(Property::VerticalAlign)
            .into_iter()
            .collect(),
        "content" => parse_content(value)
            .map(Property::Content)
            .into_iter()
            .collect(),
        "flex-direction" => parse_flex_direction(value)
            .map(Property::FlexDirection)
            .into_iter()
            .collect(),
        "flex-wrap" => parse_flex_wrap(value)
            .map(Property::FlexWrap)
            .into_iter()
            .collect(),
        "justify-content" => parse_justify(value)
            .map(Property::Justify)
            .into_iter()
            .collect(),
        "align-items" => parse_align_items(value)
            .map(Property::AlignItems)
            .into_iter()
            .collect(),
        "align-self" => parse_align_self(value)
            .map(Property::AlignSelf)
            .into_iter()
            .collect(),
        // 分轴写法只取第一个长度；两值 `gap: 4px 8px` 也取第一个。
        "gap" | "row-gap" | "column-gap" | "grid-gap" => value
            .split_whitespace()
            .next()
            .and_then(parse_length)
            .map(Property::Gap)
            .into_iter()
            .collect(),
        "flex-grow" => value
            .trim()
            .parse::<f32>()
            .ok()
            .map(Property::FlexGrow)
            .into_iter()
            .collect(),
        "flex-shrink" => value
            .trim()
            .parse::<f32>()
            .ok()
            .map(Property::FlexShrink)
            .into_iter()
            .collect(),
        "flex-basis" => parse_length(value)
            .map(Property::FlexBasis)
            .into_iter()
            .collect(),
        "flex" => parse_flex_shorthand(value),
        _ => Vec::new(),
    }
}

fn parse_flex_direction(value: &str) -> Option<FlexDirection> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "row" => FlexDirection::Row,
        "row-reverse" => FlexDirection::RowReverse,
        "column" => FlexDirection::Column,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => return None,
    })
}

fn parse_flex_wrap(value: &str) -> Option<FlexWrap> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "nowrap" => FlexWrap::NoWrap,
        "wrap" => FlexWrap::Wrap,
        "wrap-reverse" => FlexWrap::WrapReverse,
        _ => return None,
    })
}

fn parse_justify(value: &str) -> Option<Justify> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "flex-start" | "start" | "left" => Justify::Start,
        "flex-end" | "end" | "right" => Justify::End,
        "center" => Justify::Center,
        "space-between" => Justify::SpaceBetween,
        "space-around" => Justify::SpaceAround,
        "space-evenly" => Justify::SpaceEvenly,
        _ => return None,
    })
}

fn parse_align_items(value: &str) -> Option<AlignItems> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "flex-start" | "start" | "self-start" => AlignItems::Start,
        "flex-end" | "end" | "self-end" => AlignItems::End,
        "center" => AlignItems::Center,
        "baseline" => AlignItems::Baseline,
        "stretch" | "normal" => AlignItems::Stretch,
        _ => return None,
    })
}

fn parse_align_self(value: &str) -> Option<AlignSelf> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "auto" => AlignSelf::Auto,
        "flex-start" | "start" | "self-start" => AlignSelf::Start,
        "flex-end" | "end" | "self-end" => AlignSelf::End,
        "center" => AlignSelf::Center,
        "baseline" => AlignSelf::Baseline,
        "stretch" | "normal" => AlignSelf::Stretch,
        _ => return None,
    })
}

fn parse_vertical_align(value: &str) -> Option<AlignSelf> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "top" | "text-top" | "super" => AlignSelf::Start,
        "middle" => AlignSelf::Center,
        "bottom" | "text-bottom" | "sub" => AlignSelf::End,
        "baseline" => AlignSelf::Baseline,
        _ => return None,
    })
}

/// `flex` 简写：`1` / `1 0 auto` / `0 0 120px` / `none`。
fn parse_flex_shorthand(value: &str) -> Vec<Property> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "none" => {
            return vec![
                Property::FlexGrow(0.0),
                Property::FlexShrink(0.0),
                Property::FlexBasis(Length::Px(0.0)),
            ];
        }
        "auto" => {
            return vec![
                Property::FlexGrow(1.0),
                Property::FlexShrink(1.0),
                Property::FlexBasis(Length::Px(0.0)),
            ];
        }
        "initial" => {
            return vec![
                Property::FlexGrow(0.0),
                Property::FlexShrink(1.0),
                Property::FlexBasis(Length::Px(0.0)),
            ];
        }
        _ => {}
    }

    let tokens: Vec<&str> = value.split_whitespace().collect();
    let mut output = Vec::new();
    let mut numbers = 0;
    for token in &tokens {
        if numbers < 2
            && let Ok(number) = token.parse::<f32>()
        {
            output.push(if numbers == 0 {
                Property::FlexGrow(number)
            } else {
                Property::FlexShrink(number)
            });
            numbers += 1;
            continue;
        }
        if let Some(length) = parse_length(token) {
            output.push(Property::FlexBasis(length));
        }
    }
    // 只写了一个数值时按规范补 `flex-shrink: 1`、`flex-basis: 0`。
    if numbers == 1 {
        output.push(Property::FlexShrink(1.0));
        output.push(Property::FlexBasis(Length::Px(0.0)));
    }
    output
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
        "inline" | "inline-flex" | "inline-grid" | "contents" => Some(Display::Inline),
        "inline-block" => Some(Display::InlineBlock),
        "flex" => Some(Display::Flex),
        "block" | "grid" | "list-item" | "table" | "table-row" | "table-cell" | "table-caption" => {
            Some(Display::Block)
        }
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
    if value.starts_with("calc(")
        || value.starts_with("min(")
        || value.starts_with("max(")
        || value.starts_with("clamp(")
    {
        return parse_math_length(&value);
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
        "q" => Length::Px(number * 96.0 / 101.6),
        "vw" => Length::Vw(number),
        "vh" => Length::Vh(number),
        "vmin" => Length::Vmin(number),
        "vmax" => Length::Vmax(number),
        "ch" => Length::Ch(number),
        "lh" => Length::Lh(number),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct MathValue {
    px: f32,
    percent: f32,
    unitless: bool,
}

impl MathValue {
    fn add(self, other: Self) -> Option<Self> {
        if self.unitless && other.unitless {
            return Some(Self {
                px: self.px + other.px,
                percent: 0.0,
                unitless: true,
            });
        }
        if self.unitless || other.unitless {
            let scalar = if self.unitless { self.px } else { other.px };
            if scalar.abs() > f32::EPSILON {
                return None;
            }
        }
        Some(Self {
            px: self.px + other.px,
            percent: self.percent + other.percent,
            unitless: false,
        })
    }

    fn sub(self, other: Self) -> Option<Self> {
        self.add(Self {
            px: -other.px,
            percent: -other.percent,
            unitless: other.unitless,
        })
    }

    fn mul(self, other: Self) -> Option<Self> {
        if self.unitless {
            return Some(Self {
                px: other.px * self.px,
                percent: other.percent * self.px,
                unitless: other.unitless,
            });
        }
        if other.unitless {
            return Some(Self {
                px: self.px * other.px,
                percent: self.percent * other.px,
                unitless: self.unitless,
            });
        }
        None
    }

    fn div(self, other: Self) -> Option<Self> {
        if !other.unitless || other.px.abs() < f32::EPSILON {
            return None;
        }
        Some(Self {
            px: self.px / other.px,
            percent: self.percent / other.px,
            unitless: self.unitless,
        })
    }

    fn length(self) -> Length {
        if self.percent.abs() < f32::EPSILON {
            Length::Px(self.px)
        } else if self.px.abs() < f32::EPSILON {
            Length::Percent(self.percent)
        } else {
            Length::Calc {
                px: self.px,
                percent: self.percent,
            }
        }
    }
}

fn parse_math_length(value: &str) -> Option<Length> {
    let mut parser = MathParser {
        input: value.as_bytes(),
        pos: 0,
    };
    let result = parser.expression()?;
    parser.skip_space();
    if parser.pos != parser.input.len() {
        return None;
    }
    Some(result.length())
}

struct MathParser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl MathParser<'_> {
    fn skip_space(&mut self) {
        while self
            .input
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_space();
        self.input.get(self.pos).copied()
    }

    fn expression(&mut self) -> Option<MathValue> {
        let mut value = self.term()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    value = value.add(self.term()?)?;
                }
                Some(b'-') => {
                    self.pos += 1;
                    value = value.sub(self.term()?)?;
                }
                _ => return Some(value),
            }
        }
    }

    fn term(&mut self) -> Option<MathValue> {
        let mut value = self.factor()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    value = value.mul(self.factor()?)?;
                }
                Some(b'/') => {
                    self.pos += 1;
                    value = value.div(self.factor()?)?;
                }
                _ => return Some(value),
            }
        }
    }

    fn factor(&mut self) -> Option<MathValue> {
        self.skip_space();
        match self.peek()? {
            b'+' => {
                self.pos += 1;
                self.factor()
            }
            b'-' => {
                self.pos += 1;
                self.factor().map(|value| MathValue {
                    px: -value.px,
                    percent: -value.percent,
                    unitless: value.unitless,
                })
            }
            b'(' => {
                self.pos += 1;
                let value = self.expression()?;
                self.skip_space();
                (self.peek()? == b')').then(|| {
                    self.pos += 1;
                    value
                })
            }
            byte if byte.is_ascii_digit() || byte == b'.' => self.number(),
            byte if byte.is_ascii_alphabetic() => self.function(),
            _ => None,
        }
    }

    fn number(&mut self) -> Option<MathValue> {
        let start = self.pos;
        while let Some(byte) = self.input.get(self.pos) {
            if byte.is_ascii_alphanumeric() || *byte == b'.' || *byte == b'%' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let token = std::str::from_utf8(&self.input[start..self.pos]).ok()?;
        if let Ok(number) = token.parse::<f32>() {
            return Some(MathValue {
                px: number,
                percent: 0.0,
                unitless: true,
            });
        }
        let length = parse_length(token)?;
        Some(match length {
            Length::Percent(value) => MathValue {
                px: 0.0,
                percent: value,
                unitless: false,
            },
            Length::Vw(value) => MathValue {
                px: value * VIEWPORT_WIDTH / 100.0,
                percent: 0.0,
                unitless: false,
            },
            Length::Vh(value) => MathValue {
                px: value * VIEWPORT_HEIGHT / 100.0,
                percent: 0.0,
                unitless: false,
            },
            Length::Vmin(value) => MathValue {
                px: value * VIEWPORT_WIDTH.min(VIEWPORT_HEIGHT) / 100.0,
                percent: 0.0,
                unitless: false,
            },
            Length::Vmax(value) => MathValue {
                px: value * VIEWPORT_WIDTH.max(VIEWPORT_HEIGHT) / 100.0,
                percent: 0.0,
                unitless: false,
            },
            // calc() 里用相对单位需要真实父字号/行高，留到布局阶段；先不支持。
            Length::Em(_) | Length::Rem(_) | Length::Ex(_) | Length::Ch(_) | Length::Lh(_) => {
                return None;
            }
            other => MathValue {
                px: other.resolve(ROOT_SIZE, ROOT_SIZE),
                percent: 0.0,
                unitless: false,
            },
        })
    }

    fn function(&mut self) -> Option<MathValue> {
        let start = self.pos;
        while self
            .input
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        {
            self.pos += 1;
        }
        let name = std::str::from_utf8(&self.input[start..self.pos]).ok()?;
        self.skip_space();
        if self.peek()? != b'(' {
            return None;
        }
        self.pos += 1;

        if name == "calc" {
            let value = self.expression()?;
            self.skip_space();
            (self.peek()? == b')').then(|| {
                self.pos += 1;
                value
            })
        } else if name == "min" || name == "max" || name == "clamp" {
            let mut values = Vec::new();
            loop {
                values.push(self.expression()?);
                match self.peek()? {
                    b',' => self.pos += 1,
                    b')' => {
                        self.pos += 1;
                        break;
                    }
                    _ => return None,
                }
            }
            let mut result = *values.first()?;
            let has_percent = values
                .iter()
                .any(|value| value.percent.abs() > f32::EPSILON);
            let has_px = values.iter().any(|value| value.px.abs() > f32::EPSILON);
            if has_percent && has_px {
                return None;
            }
            if name == "clamp" && values.len() == 3 {
                let min = values[0];
                let preferred = values[1];
                let max = values[2];
                result = if preferred.px < min.px {
                    min
                } else {
                    preferred
                };
                result = if result.px > max.px { max } else { result };
                return Some(result);
            }
            for value in values.into_iter().skip(1) {
                result = if name == "min" {
                    if value.px < result.px { value } else { result }
                } else if value.px > result.px {
                    value
                } else {
                    result
                };
            }
            Some(result)
        } else {
            None
        }
    }
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
    if lower.starts_with("hsl") {
        return parse_hsl(&lower);
    }
    named_color(&lower)
}

fn parse_hsl(value: &str) -> Option<Color> {
    let open = value.find('(')?;
    let close = value.rfind(')')?;
    let parts: Vec<&str> = value[open + 1..close]
        .split([',', '/'])
        .flat_map(str::split_whitespace)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() < 3 {
        return None;
    }

    let hue = parts[0]
        .trim_end_matches("deg")
        .trim_end_matches("grad")
        .parse::<f32>()
        .ok()?;
    let hue = ((hue % 360.0) + 360.0) % 360.0;
    let saturation = percentage(parts[1])?;
    let lightness = percentage(parts[2])?;
    let alpha = match parts.get(3) {
        Some(part) => (percentage(part)? * 255.0).round() as u8,
        None => 255,
    };

    let (red, green, blue) = hsl_to_rgb(hue, saturation, lightness);
    Some(Color::rgba(red, green, blue, alpha))
}

fn percentage(value: &str) -> Option<f32> {
    let value = value.trim();
    if let Some(value) = value.strip_suffix('%') {
        return Some(value.trim().parse::<f32>().ok()? / 100.0);
    }
    value.parse::<f32>().ok()
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (u8, u8, u8) {
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let segment = hue / 60.0;
    let x = chroma * (1.0 - (segment % 2.0 - 1.0).abs());
    let (red, green, blue) = match segment as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = lightness - chroma / 2.0;
    let channel = |value: f32| ((value + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (channel(red), channel(green), channel(blue))
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
        "aliceblue" => Color::rgb(240, 248, 255),
        "antiquewhite" => Color::rgb(250, 235, 215),
        "aquamarine" => Color::rgb(127, 255, 212),
        "azure" => Color::rgb(240, 255, 255),
        "beige" => Color::rgb(245, 245, 220),
        "bisque" => Color::rgb(255, 228, 196),
        "blanchedalmond" => Color::rgb(255, 235, 205),
        "blueviolet" => Color::rgb(138, 43, 226),
        "burlywood" => Color::rgb(222, 184, 135),
        "cadetblue" => Color::rgb(95, 158, 160),
        "chartreuse" => Color::rgb(127, 255, 0),
        "chocolate" => Color::rgb(210, 105, 30),
        "coral" => Color::rgb(255, 127, 80),
        "cornflowerblue" => Color::rgb(100, 149, 237),
        "cornsilk" => Color::rgb(255, 248, 220),
        "crimson" => Color::rgb(220, 20, 60),
        "darkcyan" => Color::rgb(0, 139, 139),
        "darkgoldenrod" => Color::rgb(184, 134, 11),
        "darkkhaki" => Color::rgb(189, 183, 107),
        "darkmagenta" => Color::rgb(139, 0, 139),
        "darkolivegreen" => Color::rgb(85, 107, 47),
        "darkorange" => Color::rgb(255, 140, 0),
        "darkorchid" => Color::rgb(153, 50, 204),
        "darksalmon" => Color::rgb(233, 150, 122),
        "darkseagreen" => Color::rgb(143, 188, 143),
        "darkslateblue" => Color::rgb(72, 61, 139),
        "darkslategray" | "darkslategrey" => Color::rgb(47, 79, 79),
        "darkturquoise" => Color::rgb(0, 206, 209),
        "darkviolet" => Color::rgb(148, 0, 211),
        "deeppink" => Color::rgb(255, 20, 147),
        "deepskyblue" => Color::rgb(0, 191, 255),
        "dodgerblue" => Color::rgb(30, 144, 255),
        "firebrick" => Color::rgb(178, 34, 34),
        "floralwhite" => Color::rgb(255, 250, 240),
        "forestgreen" => Color::rgb(34, 139, 34),
        "gainsboro" => Color::rgb(220, 220, 220),
        "ghostwhite" => Color::rgb(248, 248, 255),
        "goldenrod" => Color::rgb(218, 165, 32),
        "greenyellow" => Color::rgb(173, 255, 47),
        "honeydew" => Color::rgb(240, 255, 240),
        "hotpink" => Color::rgb(255, 105, 180),
        "indianred" => Color::rgb(205, 92, 92),
        "indigo" => Color::rgb(75, 0, 130),
        "ivory" => Color::rgb(255, 255, 240),
        "khaki" => Color::rgb(240, 230, 140),
        "lavender" => Color::rgb(230, 230, 250),
        "lavenderblush" => Color::rgb(255, 240, 245),
        "lawngreen" => Color::rgb(124, 252, 0),
        "lemonchiffon" => Color::rgb(255, 250, 205),
        "lightblue" => Color::rgb(173, 216, 230),
        "lightcoral" => Color::rgb(240, 128, 128),
        "lightcyan" => Color::rgb(224, 255, 255),
        "lightgoldenrodyellow" => Color::rgb(250, 250, 210),
        "lightgreen" => Color::rgb(144, 238, 144),
        "lightpink" => Color::rgb(255, 182, 193),
        "lightsalmon" => Color::rgb(255, 160, 122),
        "lightseagreen" => Color::rgb(32, 178, 170),
        "lightskyblue" => Color::rgb(135, 206, 250),
        "lightslategray" | "lightslategrey" => Color::rgb(119, 136, 153),
        "lightsteelblue" => Color::rgb(176, 196, 222),
        "lightyellow" => Color::rgb(255, 255, 224),
        "limegreen" => Color::rgb(50, 205, 50),
        "linen" => Color::rgb(250, 240, 230),
        "mediumaquamarine" => Color::rgb(102, 205, 170),
        "mediumblue" => Color::rgb(0, 0, 205),
        "mediumorchid" => Color::rgb(186, 85, 211),
        "mediumpurple" => Color::rgb(147, 112, 219),
        "mediumseagreen" => Color::rgb(60, 179, 113),
        "mediumslateblue" => Color::rgb(123, 104, 238),
        "mediumspringgreen" => Color::rgb(0, 250, 154),
        "mediumturquoise" => Color::rgb(72, 209, 204),
        "mediumvioletred" => Color::rgb(199, 21, 133),
        "midnightblue" => Color::rgb(25, 25, 112),
        "mintcream" => Color::rgb(245, 255, 250),
        "mistyrose" => Color::rgb(255, 228, 225),
        "moccasin" => Color::rgb(255, 228, 181),
        "navajowhite" => Color::rgb(255, 222, 173),
        "oldlace" => Color::rgb(253, 245, 230),
        "olivedrab" => Color::rgb(107, 142, 35),
        "orangered" => Color::rgb(255, 69, 0),
        "orchid" => Color::rgb(218, 112, 214),
        "palegoldenrod" => Color::rgb(238, 232, 170),
        "palegreen" => Color::rgb(152, 251, 152),
        "paleturquoise" => Color::rgb(175, 238, 238),
        "palevioletred" => Color::rgb(219, 112, 147),
        "papayawhip" => Color::rgb(255, 239, 213),
        "peachpuff" => Color::rgb(255, 218, 185),
        "peru" => Color::rgb(205, 133, 63),
        "plum" => Color::rgb(221, 160, 221),
        "powderblue" => Color::rgb(176, 224, 230),
        "rebeccapurple" => Color::rgb(102, 51, 153),
        "rosybrown" => Color::rgb(188, 143, 143),
        "royalblue" => Color::rgb(65, 105, 225),
        "saddlebrown" => Color::rgb(139, 69, 19),
        "salmon" => Color::rgb(250, 128, 114),
        "sandybrown" => Color::rgb(244, 164, 96),
        "seagreen" => Color::rgb(46, 139, 87),
        "seashell" => Color::rgb(255, 245, 238),
        "sienna" => Color::rgb(160, 82, 45),
        "skyblue" => Color::rgb(135, 206, 235),
        "slateblue" => Color::rgb(106, 90, 205),
        "slategray" | "slategrey" => Color::rgb(112, 128, 144),
        "snow" => Color::rgb(255, 250, 250),
        "springgreen" => Color::rgb(0, 255, 127),
        "steelblue" => Color::rgb(70, 130, 180),
        "tan" => Color::rgb(210, 180, 140),
        "thistle" => Color::rgb(216, 191, 216),
        "tomato" => Color::rgb(255, 99, 71),
        "turquoise" => Color::rgb(64, 224, 208),
        "violet" => Color::rgb(238, 130, 238),
        "wheat" => Color::rgb(245, 222, 179),
        "whitesmoke" => Color::rgb(245, 245, 245),
        "yellowgreen" => Color::rgb(154, 205, 50),
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
    fn inline_block_is_an_atomic_inline_box() {
        let sheet = Stylesheet::parse(".target { display: inline-block; }");
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).display,
            Some(Display::InlineBlock)
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
    fn flex_properties_are_parsed() {
        let sheet = Stylesheet::parse(
            ".target { display: flex; flex-direction: column-reverse; flex-wrap: wrap;
                       justify-content: space-evenly; align-items: baseline;
                       gap: 8px 12px; flex: 0 0 140px; align-self: center; }",
        );
        let (_doc, element) = document(r#"<div class="target">x</div>"#);
        let style = computed_for(&sheet, &element, None);
        assert_eq!(style.display, Some(Display::Flex));
        assert_eq!(style.flex_direction, Some(FlexDirection::ColumnReverse));
        assert_eq!(style.flex_wrap, Some(FlexWrap::Wrap));
        assert_eq!(style.justify, Some(Justify::SpaceEvenly));
        assert_eq!(style.align_items, Some(AlignItems::Baseline));
        assert_eq!(style.gap, Some(8.0));
        assert_eq!(style.flex_grow, Some(0.0));
        assert_eq!(style.flex_shrink, Some(0.0));
        assert_eq!(style.flex_basis, Some(140.0));
        assert_eq!(style.align_self, Some(AlignSelf::Center));
    }

    #[test]
    fn flex_shorthand_expands_single_number() {
        let properties = parse_flex_shorthand("1");
        assert!(matches!(properties[0], Property::FlexGrow(1.0)));
        assert!(matches!(properties[1], Property::FlexShrink(1.0)));
        assert!(matches!(
            properties[2],
            Property::FlexBasis(Length::Px(0.0))
        ));
        assert!(matches!(
            parse_flex_shorthand("none")[0],
            Property::FlexGrow(0.0)
        ));
    }

    #[test]
    fn display_flex_is_not_flattened() {
        let sheet = Stylesheet::parse(".target { display: flex; }");
        let (_doc, element) = document(r#"<div class="target">x</div>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).display,
            Some(Display::Flex)
        );
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
        assert_eq!(
            parse_color("hsl(0, 100%, 50%)"),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            parse_color("hsl(120 100% 25%)"),
            Some(Color::rgb(0, 128, 0))
        );
    }

    #[test]
    fn media_rules_use_the_dictionary_viewport() {
        let sheet = Stylesheet::parse(
            ".target { color: red; }
             @media (min-width: 400px) { .target { color: blue; } }
             @media (max-width: 100px) { .target { color: lime; } }",
        );
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).color,
            Some(Color::rgb(0, 0, 255))
        );
    }

    #[test]
    fn supports_rules_are_evaluated() {
        let sheet = Stylesheet::parse(
            ".target { color: red; }
             @supports (display: flex) { .target { color: blue; } }
             @supports (grid-template-columns: 1fr) { .target { color: lime; } }",
        );
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).color,
            Some(Color::rgb(0, 0, 255))
        );
    }

    #[test]
    fn layer_rules_are_flattened_in_order() {
        let sheet = Stylesheet::parse(
            ".target { color: red; }
             @layer base { .target { color: blue; } }",
        );
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        assert_eq!(
            computed_for(&sheet, &element, None).color,
            Some(Color::rgb(0, 0, 255))
        );
    }

    #[test]
    fn variables_inherit_and_support_fallbacks() {
        let sheet = Stylesheet::parse(
            "html { --brand: #123456; }
             .target {
                 color: var(--brand);
                 font-size: var(--missing, 14px);
                 border-bottom: 1px solid var(--brand);
             }",
        );
        let document =
            kuchiki::parse_html().one(r#"<html><body><span class="target">x</span></body></html>"#);
        let root = document.select_first("html").unwrap();
        let mut root_style = Computed::default();
        root_style.apply(&sheet.matched(&root), &[], ROOT_SIZE);

        let target = document.select_first(".target").unwrap();
        let mut style = Computed::inherited(&root_style);
        style.apply(&sheet.matched(&target), &[], ROOT_SIZE);

        assert_eq!(style.color, Some(Color::rgb(0x12, 0x34, 0x56)));
        assert_eq!(style.size, Some(14.0));
        assert_eq!(
            style.border_bottom.and_then(|border| border.color),
            Some(Color::rgb(0x12, 0x34, 0x56))
        );
    }

    #[test]
    fn calc_min_max_and_clamp_are_evaluated() {
        let sheet = Stylesheet::parse(
            ".target {
                 font-size: calc(10px + 2px);
                 width: min(100px, 200px);
                 height: clamp(10px, 50px, 30px);
             }",
        );
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        let style = computed_for(&sheet, &element, None);
        assert_eq!(style.size, Some(12.0));
        assert_eq!(style.width, Some(100.0));
        assert_eq!(style.height, Some(30.0));
    }

    #[test]
    fn current_color_resolves_against_the_computed_color() {
        let sheet = Stylesheet::parse(
            ".target { color: rgb(255, 0, 0); border-bottom: 2px solid currentColor; }",
        );
        let (_doc, element) = document(r#"<span class="target">x</span>"#);
        let style = computed_for(&sheet, &element, None);
        assert_eq!(
            style.border_bottom.and_then(|border| border.color),
            Some(Color::rgb(255, 0, 0))
        );
    }

    #[test]
    fn visibility_hidden_can_be_overridden() {
        let sheet = Stylesheet::parse(
            ".target { visibility: hidden; }
             .target span { visibility: visible; }",
        );
        let document = kuchiki::parse_html()
            .one(r#"<html><body><div class="target"><span>x</span></div></body></html>"#);
        let target = document.select_first(".target").unwrap();
        let parent = Computed::default();
        let mut parent_style = Computed::inherited(&parent);
        parent_style.apply(&sheet.matched(&target), &[], ROOT_SIZE);
        assert!(parent_style.hidden);

        let child = document.select_first(".target span").unwrap();
        let mut child_style = Computed::inherited(&parent_style);
        child_style.apply(&sheet.matched(&child), &[], ROOT_SIZE);
        assert!(!child_style.hidden);
    }

    #[test]
    fn stats_count_unsupported_properties_and_at_rules() {
        let sheet = Stylesheet::parse(
            "@media screen { .target { float: left; letter-spacing: 1px; color: var(--c); } }",
        );
        let stats = sheet.stats();
        assert_eq!(stats.at_rules.get("media"), Some(&1));
        assert!(!stats.unsupported_declarations.contains_key("float"));
        assert_eq!(
            stats.unsupported_declarations.get("letter-spacing"),
            Some(&1)
        );
        assert!(!stats.unsupported_declarations.contains_key("color"));
    }
}
