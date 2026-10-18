//! 词典 worker：加载 `.mdx`（及同名 `.mdd`、外置 CSS/字体）并响应查询。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;
use mdictlib::{MddFile, MdxFile};

use crate::css::Stylesheet;
use crate::fonts::FontLibrary;
use crate::html::{self, RenderContext, ResourceResolver};
use crate::model::DictEntry;

/// 单次查询最多跟随几层 `@@@LINK` 重定向。
const MAX_REDIRECTS: usize = 3;
/// 释义正文最多保留多少字符，避免超大词条拖慢 UI。
const MAX_DEFINITION_CHARS: usize = 60_000;
/// 启动时最多预读多少个 `.mdd` 里的样式表。
const MAX_CSS_FILES: usize = 64;
/// 预读样式表的总字节上限。
const MAX_CSS_BYTES: usize = 4 * 1024 * 1024;
/// 字体总字节上限。
const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
/// 图片 / 样式表缓存条目上限。
const MAX_CACHE_ENTRIES: usize = 512;
/// 一次前缀联想最多回传多少个词（跨词典合并、去重后）。
const MAX_PREFIX_SUGGESTIONS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DictionaryConfig {
    pub path: PathBuf,
    pub enabled: bool,
}

/// 一本词典的加载结果，随 `Configured` 事件回传。
#[derive(Clone, Debug)]
pub struct DictionaryInfo {
    pub path: PathBuf,
    pub title: Option<String>,
    pub entries: u64,
    pub enabled: bool,
    pub fonts: Vec<Vec<u8>>,
    pub error: Option<String>,
}

pub enum Command {
    /// 用完整的词典列表重建 worker 状态：保留已加载的词典，按新顺序与启用状态
    /// 重排；停用的词典会被卸载。
    Configure {
        request: u64,
        dictionaries: Vec<DictionaryConfig>,
    },
    Unload,
    Lookup {
        request: u64,
        term: String,
    },
    /// 跨词典前缀联想；`limit` 是每本词典最多取多少条原始键。
    Prefix {
        request: u64,
        term: String,
        limit: usize,
    },
}

pub enum Event {
    Configured {
        request: u64,
        dictionaries: Vec<DictionaryInfo>,
    },
    Found {
        request: u64,
        term: String,
        entries: Vec<DictEntry>,
        redirected: bool,
    },
    NotFound {
        request: u64,
        term: String,
    },
    PrefixSuggestions {
        request: u64,
        term: String,
        suggestions: Vec<String>,
    },
    Error {
        request: Option<u64>,
        message: String,
    },
}

pub struct DictEngine {
    pub commands: Sender<Command>,
}

impl DictEngine {
    pub fn spawn(events: UnboundedSender<Event>) -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("mdx-worker".into())
            .spawn(move || worker_loop(receiver, events))
            .expect("failed to spawn mdx worker");
        Self { commands }
    }
}

struct LoadedDictionary {
    path: PathBuf,
    title: Option<String>,
    entries: u64,
    fonts: Vec<Vec<u8>>,
    dictionary: Dictionary,
}

impl LoadedDictionary {
    fn info(&self, enabled: bool) -> DictionaryInfo {
        DictionaryInfo {
            path: self.path.clone(),
            title: self.title.clone(),
            entries: self.entries,
            enabled,
            fonts: self.fonts.clone(),
            error: None,
        }
    }

    fn source(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.display().to_string())
        })
    }
}

fn worker_loop(commands: Receiver<Command>, events: UnboundedSender<Event>) {
    let mut loaded: Vec<LoadedDictionary> = Vec::new();

    while let Ok(command) = commands.recv() {
        match command {
            Command::Configure {
                request,
                dictionaries,
            } => {
                let mut previous: HashMap<PathBuf, LoadedDictionary> = loaded
                    .drain(..)
                    .map(|dictionary| (dictionary.path.clone(), dictionary))
                    .collect();
                let mut next = Vec::new();
                let mut infos = Vec::new();
                let mut seen = HashSet::new();

                for config in dictionaries {
                    if !seen.insert(config.path.clone()) {
                        continue;
                    }
                    if !config.enabled {
                        let (entries, title) = previous
                            .get(&config.path)
                            .map(|dictionary| (dictionary.entries, dictionary.title.clone()))
                            .unwrap_or((0, None));
                        infos.push(DictionaryInfo {
                            path: config.path,
                            title,
                            entries,
                            enabled: false,
                            fonts: Vec::new(),
                            error: None,
                        });
                        continue;
                    }

                    let dictionary = previous.remove(&config.path).or_else(|| {
                        match load_dictionary(&config.path) {
                            Ok((dictionary, title, entries)) => {
                                log::info!(
                                    "loaded dictionary {} ({} entries, {} style rules, {} fonts, {} mdds)",
                                    config.path.display(),
                                    entries,
                                    dictionary.stylesheet.len(),
                                    dictionary.fonts.faces().len(),
                                    dictionary.mdd.len(),
                                );
                                let fonts = dictionary.fonts.bytes();
                                Some(LoadedDictionary {
                                    path: config.path.clone(),
                                    title,
                                    entries,
                                    fonts,
                                    dictionary,
                                })
                            }
                            Err(error) => {
                                let message = format!("加载词典失败: {error:#}");
                                log::error!("{message}");
                                infos.push(DictionaryInfo {
                                    path: config.path,
                                    title: None,
                                    entries: 0,
                                    enabled: true,
                                    fonts: Vec::new(),
                                    error: Some(message),
                                });
                                None
                            }
                        }
                    });

                    if let Some(dictionary) = dictionary {
                        infos.push(dictionary.info(true));
                        next.push(dictionary);
                    }
                }

                loaded = next;
                let _ = events.unbounded_send(Event::Configured {
                    request,
                    dictionaries: infos,
                });
            }
            Command::Unload => {
                loaded.clear();
            }
            Command::Lookup { request, term } => {
                if loaded.is_empty() {
                    let _ = events.unbounded_send(Event::Error {
                        request: Some(request),
                        message: "词典还没有加载完成".to_string(),
                    });
                    continue;
                }

                let mut entries = Vec::new();
                let mut redirected = false;
                for dictionary in &mut loaded {
                    match dictionary.dictionary.lookup(&term) {
                        Ok(Some((mut entry, entry_redirected))) => {
                            entry.source = Some(dictionary.source());
                            entries.push(entry);
                            redirected |= entry_redirected;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let message = format!("查询词典失败: {error:#}");
                            log::error!("{message}");
                        }
                    }
                }

                if entries.is_empty() {
                    let _ = events.unbounded_send(Event::NotFound { request, term });
                } else {
                    let _ = events.unbounded_send(Event::Found {
                        request,
                        term,
                        entries,
                        redirected,
                    });
                }
            }
            Command::Prefix {
                request,
                term,
                limit,
            } => {
                let suggestions = if term.trim().is_empty() {
                    Vec::new()
                } else {
                    prefix_suggestions(&loaded, term.trim(), limit)
                };
                let _ = events.unbounded_send(Event::PrefixSuggestions {
                    request,
                    term,
                    suggestions,
                });
            }
        }
    }
}

/// 跨词典收集前缀建议：保持词典顺序、忽略大小写去重，最多 8 条。
///
/// 精确前缀没有结果时，逐个去掉末尾字符再试（至少保留一个字符），给出
/// “近似前缀”建议；这比全表模糊扫描便宜，输入末端拼错时仍能命中。
fn prefix_suggestions(
    dictionaries: &[LoadedDictionary],
    term: &str,
    per_dictionary: usize,
) -> Vec<String> {
    if per_dictionary == 0 {
        return Vec::new();
    }

    let mut suggestions = collect_prefixes(dictionaries, term, per_dictionary);
    if !suggestions.is_empty() {
        return suggestions;
    }

    let mut fallback = term.to_string();
    while fallback.chars().count() > 1 {
        let Some((index, _)) = fallback.char_indices().next_back() else {
            break;
        };
        fallback.truncate(index);
        suggestions = collect_prefixes(dictionaries, &fallback, per_dictionary);
        if !suggestions.is_empty() {
            break;
        }
    }
    suggestions
}

fn collect_prefixes(
    dictionaries: &[LoadedDictionary],
    term: &str,
    per_dictionary: usize,
) -> Vec<String> {
    let mut suggestions = Vec::new();
    let mut seen = HashSet::new();
    for dictionary in dictionaries {
        let keys = match dictionary.dictionary.mdx.prefix_keys(term, per_dictionary) {
            Ok(keys) => keys,
            Err(error) => {
                log::warn!("前缀联想失败 {}: {error:#}", dictionary.path.display());
                continue;
            }
        };
        for key in keys {
            let value = key.key().to_string();
            if seen.insert(value.to_lowercase()) {
                suggestions.push(value);
                if suggestions.len() == MAX_PREFIX_SUGGESTIONS {
                    return suggestions;
                }
            }
        }
    }
    suggestions
}

/// 打开后的词典：MDX 词条 + 同名 MDD 资源 + 样式/字体。
struct Dictionary {
    mdx: MdxFile,
    mdd: Vec<MddFile>,
    base_dir: PathBuf,
    stylesheet: Stylesheet,
    fonts: FontLibrary,
    loaded_stylesheets: Vec<String>,
    resources: HashMap<String, Option<Arc<[u8]>>>,
    stylesheets: HashMap<String, Option<String>>,
}

impl Dictionary {
    fn lookup(&mut self, term: &str) -> Result<Option<(DictEntry, bool)>> {
        let mut seen = HashSet::new();
        seen.insert(term.to_lowercase());

        let Some((mut headword, mut body)) = find_entry(&self.mdx, term)? else {
            return Ok(None);
        };

        let mut redirected = false;
        for _ in 0..MAX_REDIRECTS {
            let Some(target) = link_target(&body) else {
                break;
            };
            if !seen.insert(target.to_lowercase()) {
                break;
            }

            match find_entry(&self.mdx, &target)? {
                Some((target_headword, target_body)) => {
                    headword = target_headword;
                    body = target_body;
                    redirected = true;
                }
                None => return Ok(None),
            }
        }

        let body = truncate_chars(&body, MAX_DEFINITION_CHARS);

        let Dictionary {
            mdd,
            base_dir,
            stylesheet,
            fonts,
            loaded_stylesheets,
            resources,
            stylesheets,
            ..
        } = self;
        let mut resolver = MddResolver {
            base_dir,
            mdd,
            resources,
            stylesheets,
        };
        let mut context = RenderContext {
            stylesheet,
            resolver: &mut resolver,
            fonts: Some(fonts),
            loaded_stylesheets,
        };
        let blocks = html::render(body, &mut context);
        let phonetic = html::extract_phonetic(&blocks);

        Ok(Some((
            DictEntry {
                headword,
                phonetic,
                blocks,
                source: None,
            },
            redirected,
        )))
    }
}

fn load_dictionary(path: &Path) -> Result<(Dictionary, Option<String>, u64)> {
    let mdx = MdxFile::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let title = mdx.header().title().map(|title| title.to_string());
    let entries = mdx.len();
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mdd = open_mdds(path);
    let (stylesheet, loaded_stylesheets) = load_stylesheet(&mdx, &base_dir, &mdd);
    let fonts = load_fonts(&stylesheet, &base_dir, &mdd);

    Ok((
        Dictionary {
            mdx,
            mdd,
            base_dir,
            stylesheet,
            fonts,
            loaded_stylesheets,
            resources: HashMap::new(),
            stylesheets: HashMap::new(),
        },
        title,
        entries,
    ))
}

/// 打开基础 `.mdd` 和拆分出来的 `.1.mdd`、`.2.mdd`……
fn open_mdds(path: &Path) -> Vec<MddFile> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let Some(stem) = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
    else {
        return Vec::new();
    };

    let mut candidates: Vec<(u32, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if !candidate.is_file() {
                continue;
            }
            let name = candidate
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let Some(order) = mdd_order(&name, &stem) else {
                continue;
            };
            candidates.push((order, candidate));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

    let mut mdds = Vec::new();
    for (_, candidate) in candidates.into_iter().take(8) {
        match MddFile::open(&candidate) {
            Ok(mdd) => {
                log::info!(
                    "opened mdd {} ({} resources)",
                    candidate.display(),
                    mdd.len()
                );
                mdds.push(mdd);
            }
            Err(error) => log::warn!("无法打开 {}: {error:#}", candidate.display()),
        }
    }
    mdds
}

/// `stem.mdd` 返回 0，`stem.1.mdd` 返回 1，用于排序；不匹配返回 `None`。
fn mdd_order(name: &str, stem: &str) -> Option<u32> {
    if name == format!("{stem}.mdd") {
        return Some(0);
    }
    let rest = name
        .strip_prefix(&format!("{stem}."))?
        .strip_suffix(".mdd")?;
    if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(rest.parse().unwrap_or(u32::MAX))
}

/// 词典级样式：`.mdd` 里的 `.css`、外部 CSS 文件和 MDX 头部 `StyleSheet`。
fn load_stylesheet(mdx: &MdxFile, base_dir: &Path, mdds: &[MddFile]) -> (Stylesheet, Vec<String>) {
    let mut stylesheet = Stylesheet::default();
    let mut loaded = Vec::new();

    let mut total = 0;
    for mdd in mdds {
        let mut files = Vec::new();
        for key in mdd.keys().flatten() {
            let name = key.key();
            if name.to_ascii_lowercase().ends_with(".css") {
                files.push((key.ordinal(), name.to_string()));
                if files.len() >= MAX_CSS_FILES {
                    break;
                }
            }
        }
        files.sort_by(|left, right| left.1.cmp(&right.1));
        for (ordinal, name) in files {
            let Ok(Some(resource)) = mdd.resource_at(ordinal) else {
                continue;
            };
            if total + resource.bytes().len() > MAX_CSS_BYTES {
                log::warn!("stylesheet budget exhausted at {name}");
                break;
            }
            total += resource.bytes().len();
            stylesheet.extend(Stylesheet::parse(&decode_text(resource.bytes())));
        }
    }

    let description = mdx.header().description().unwrap_or_default();
    for href in css_links(description) {
        let css = read_external_css(base_dir, &href)
            .or_else(|| lookup_mdd_resource(mdds, &href).map(|bytes| decode_text(&bytes)));
        if let Some(css) = css {
            stylesheet.extend(Stylesheet::parse(&css));
            loaded.push(href);
        } else {
            log::warn!("找不到词典样式表 {href}");
        }
    }

    for (name, value) in mdx.header().attributes() {
        if name.eq_ignore_ascii_case("stylesheet") && !value.trim().is_empty() {
            stylesheet.extend(Stylesheet::parse(value));
        }
    }

    (stylesheet, loaded)
}

/// 从头部描述里找 `<link href="...css">`。
fn css_links(text: &str) -> Vec<String> {
    let mut output = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut offset = 0;
    while let Some(index) = rest.find("href=") {
        let value_start = offset + index + "href=".len();
        let after = &text[value_start..];
        let (value, consumed) = if let Some(after) = after.strip_prefix('"') {
            (after.split('"').next().unwrap_or_default(), true)
        } else if let Some(after) = after.strip_prefix('\'') {
            (after.split('\'').next().unwrap_or_default(), true)
        } else {
            (after.split(['>', ' ']).next().unwrap_or_default(), false)
        };
        let value = value.trim();
        if value.to_ascii_lowercase().ends_with(".css") && !value.is_empty() {
            output.push(value.to_string());
        }
        if !consumed && value.is_empty() {
            break;
        }
        let advance = index + "href=".len() + value.len();
        if advance >= rest.len() {
            break;
        }
        offset += advance;
        rest = &lower[offset..];
    }
    output
}

fn load_fonts(stylesheet: &Stylesheet, base_dir: &Path, mdds: &[MddFile]) -> FontLibrary {
    let mut library = FontLibrary::new();
    let mut total = 0;
    for face in stylesheet.font_faces() {
        for source in &face.sources {
            if source.starts_with("data:") {
                continue;
            }
            let bytes = read_external(base_dir, source)
                .and_then(|path| std::fs::read(path).ok())
                .or_else(|| lookup_mdd_resource(mdds, source).map(|bytes| bytes.to_vec()));
            let Some(bytes) = bytes else {
                continue;
            };
            if total + bytes.len() > MAX_FONT_BYTES {
                log::warn!("字体总大小超限，跳过 {}", face.family);
                break;
            }
            if library.add(&face.family, &bytes) {
                total += bytes.len();
                log::info!(
                    "loaded font {} ({} bytes, {} ranges) from {}",
                    face.family,
                    bytes.len(),
                    library.glyph_ranges(&face.family),
                    source
                );
                break;
            }
        }
    }
    if library.is_empty() && !stylesheet.font_faces().is_empty() {
        log::warn!(
            "词典声明了 {} 个 @font-face，但没有加载到可用字体",
            stylesheet.font_faces().len()
        );
    }
    library
}

fn read_external_css(base_dir: &Path, href: &str) -> Option<String> {
    let path = read_external(base_dir, href)?;
    let bytes = std::fs::read(path).ok()?;
    Some(decode_text(&bytes))
}

fn read_external(base_dir: &Path, href: &str) -> Option<PathBuf> {
    let path = normalized_resource(href)?;
    let candidate = base_dir.join(&path);
    candidate.is_file().then_some(candidate)
}

/// 归一化 `./foo`、`\foo`、`foo/../bar` 这类资源路径。
fn normalized_resource(value: &str) -> Option<String> {
    let decoded = percent_decode(value);
    let decoded = decoded.split(['?', '#']).next().unwrap_or(&decoded).trim();
    let mut path = decoded.replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_string();
    }
    let path = path.trim_start_matches('/').to_string();
    if path.is_empty() || path.split('/').any(|part| part == "..") {
        return None;
    }
    Some(path)
}

/// 在所有 MDD 里找资源，兼容 `/img/a.png`、`\img\a.png`、大小写不一致等写法。
fn lookup_mdd_resource(mdds: &[MddFile], src: &str) -> Option<Arc<[u8]>> {
    let path = normalized_resource(src)?;
    let backslash = path.replace('/', "\\");
    let candidates = [
        path.clone(),
        format!("\\{backslash}"),
        format!("/{path}"),
        backslash.clone(),
        format!("\\{path}"),
        path.to_ascii_lowercase(),
        format!("\\{}", backslash.to_ascii_lowercase()),
        backslash.to_ascii_lowercase(),
    ];

    for mdd in mdds {
        for candidate in &candidates {
            if let Ok(Some(resource)) = mdd.lookup(candidate) {
                return Some(Arc::from(resource.bytes().to_vec()));
            }
        }
    }
    None
}

fn normalize_key(value: &str) -> String {
    normalized_resource(value)
        .unwrap_or_default()
        .to_ascii_lowercase()
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

struct MddResolver<'a> {
    base_dir: &'a Path,
    mdd: &'a [MddFile],
    resources: &'a mut HashMap<String, Option<Arc<[u8]>>>,
    stylesheets: &'a mut HashMap<String, Option<String>>,
}

impl ResourceResolver for MddResolver<'_> {
    fn stylesheet(&mut self, href: &str) -> Option<String> {
        if href.is_empty() || href.to_ascii_lowercase().starts_with("http") {
            return None;
        }
        let key = normalize_key(href);
        if let Some(cached) = self.stylesheets.get(&key) {
            return cached.clone();
        }
        let value = read_external_css(self.base_dir, href)
            .or_else(|| lookup_mdd_resource(self.mdd, href).map(|bytes| decode_text(&bytes)));
        if self.stylesheets.len() >= MAX_CACHE_ENTRIES {
            self.stylesheets.clear();
        }
        self.stylesheets.insert(key, value.clone());
        value
    }

    fn resource(&mut self, path: &str) -> Option<Arc<[u8]>> {
        if path.is_empty() || path.to_ascii_lowercase().starts_with("http") {
            return None;
        }
        let key = normalize_key(path);
        if let Some(cached) = self.resources.get(&key) {
            return cached.clone();
        }
        let value = read_external(self.base_dir, path)
            .and_then(|path| std::fs::read(path).ok())
            .map(|bytes| Arc::from(bytes))
            .or_else(|| lookup_mdd_resource(self.mdd, path));
        if self.resources.len() >= MAX_CACHE_ENTRIES {
            self.resources.clear();
        }
        self.resources.insert(key, value.clone());
        value
    }
}

/// CSS 文件可能是 UTF-8（带/不带 BOM）或 UTF-16。
fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(rest, false);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| {
            if little_endian {
                u16::from_le_bytes([chunk[0], chunk[1]])
            } else {
                u16::from_be_bytes([chunk[0], chunk[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

fn find_entry(dictionary: &MdxFile, term: &str) -> Result<Option<(String, String)>> {
    for candidate in candidates(term) {
        if let Some(entry) = dictionary
            .lookup(&candidate)
            .with_context(|| format!("查询 {candidate:?} 失败"))?
        {
            return Ok(Some((entry.key().to_string(), entry.text().to_string())));
        }
    }
    Ok(None)
}

/// 大小写与所有格变体：原文、小写、大写、首字母大写、去掉 `'s`。
fn candidates(term: &str) -> Vec<String> {
    let mut output: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !value.is_empty() && !output.iter().any(|existing| existing == &value) {
            output.push(value);
        }
    };

    push(term.to_string());
    push(term.to_lowercase());
    push(term.to_uppercase());
    push(capitalize(term));

    if let Some(base) = term.strip_suffix("'s").or_else(|| term.strip_suffix("’s")) {
        push(base.to_string());
        push(base.to_lowercase());
    }

    output
}

fn capitalize(term: &str) -> String {
    let mut chars = term.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first
        .to_uppercase()
        .chain(chars.flat_map(char::to_lowercase))
        .collect()
}

/// `@@@LINK=word` → `word`。
fn link_target(body: &str) -> Option<String> {
    let body = body.trim_start();
    let target = body.strip_prefix("@@@LINK=")?;
    let target = target
        .split(['\n', '\r', '<'])
        .next()
        .unwrap_or_default()
        .trim();
    (!target.is_empty()).then(|| target.to_string())
}

fn truncate_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, Span};
    use futures::StreamExt;
    use mdictlib::{MddBuilder, MdxBuilder, WriteEncoding, WriteOptions};

    #[test]
    fn candidates_cover_case_and_possessive_variants() {
        let values = candidates("Apple's");
        assert_eq!(values[0], "Apple's");
        assert!(values.contains(&"apple's".to_string()));
        assert!(values.contains(&"APPLE'S".to_string()));
        assert!(values.contains(&"Apple".to_string()));
        assert!(values.contains(&"apple".to_string()));
    }

    #[test]
    fn link_target_parses_redirect() {
        assert_eq!(
            link_target("@@@LINK=hello\r\n<p>x</p>").as_deref(),
            Some("hello")
        );
        assert_eq!(link_target("<p>plain</p>"), None);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_chars("你好世界", 2), "你好");
        assert_eq!(truncate_chars("abc", 10), "abc");
    }

    #[test]
    fn css_links_are_read_from_header_description() {
        let text =
            r#"<link href="王力.css" rel="stylesheet"><bar top><a href="entry://WLa">封面</a>"#;
        assert_eq!(css_links(text), vec!["王力.css".to_string()]);
    }

    #[test]
    fn resource_paths_are_normalized() {
        assert_eq!(
            normalized_resource("./WL-special.woff").as_deref(),
            Some("WL-special.woff")
        );
        assert_eq!(
            normalized_resource("\\img\\a.png").as_deref(),
            Some("img/a.png")
        );
        assert_eq!(normalized_resource("../secret.css"), None);
    }

    #[test]
    fn worker_loads_and_queries_a_synthetic_dictionary() {
        let mut builder =
            MdxBuilder::with_options(WriteOptions::new().with_encoding(WriteEncoding::Utf8));
        builder
            .header_attribute("Title", "Test Dictionary")
            .unwrap();
        builder
            .add_entry(
                "hello",
                r#"<link rel="stylesheet" href="style.css">
                   <div class="hwd">hello</div>
                   <div class="def">你好，<b>世界</b> <mark>标记</mark> <img src="img/dot.png" width="16"></div>"#,
            )
            .unwrap();
        builder.add_entry("redirect", "@@@LINK=hello").unwrap();

        let mut bytes = Vec::new();
        builder.finish(&mut bytes).unwrap();

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let stem = format!("gpui-pdf-dict-test-{}-{stamp}", std::process::id());
        let mdx_path = std::env::temp_dir().join(format!("{stem}.mdx"));
        let mdd_path = std::env::temp_dir().join(format!("{stem}.mdd"));
        std::fs::write(&mdx_path, &bytes).unwrap();

        let png = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut png_bytes = Vec::new();
        image::DynamicImage::ImageRgba8(png)
            .write_to(
                &mut std::io::Cursor::new(&mut png_bytes),
                image::ImageFormat::Png,
            )
            .unwrap();

        let mut mdd = MddBuilder::new();
        mdd.add_resource(
            "/style.css",
            r#".hwd { font-size: 20px; color: #0000ff; } .def { color: #333333; } mark::before { content: "※"; }"#
                .as_bytes(),
        )
        .unwrap();
        mdd.add_resource("/img/dot.png", &png_bytes).unwrap();
        let mut mdd_file = std::fs::File::create(&mdd_path).unwrap();
        mdd.finish(&mut mdd_file).unwrap();
        drop(mdd_file);

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let engine = DictEngine::spawn(tx);
        engine
            .commands
            .send(Command::Configure {
                request: 1,
                dictionaries: vec![DictionaryConfig {
                    path: mdx_path.clone(),
                    enabled: true,
                }],
            })
            .unwrap();

        match futures::executor::block_on(rx.next()) {
            Some(Event::Configured { dictionaries, .. }) => {
                assert_eq!(dictionaries.len(), 1);
                assert_eq!(dictionaries[0].entries, 2);
                assert_eq!(dictionaries[0].title.as_deref(), Some("Test Dictionary"));
                assert!(dictionaries[0].error.is_none());
            }
            _ => panic!("expected Configured event"),
        }

        engine
            .commands
            .send(Command::Lookup {
                request: 7,
                term: "HELLO".to_string(),
            })
            .unwrap();

        match futures::executor::block_on(rx.next()) {
            Some(Event::Found {
                request,
                entries,
                redirected,
                ..
            }) => {
                assert_eq!(request, 7);
                assert!(!redirected);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].headword, "hello");
                assert_eq!(entries[0].source.as_deref(), Some("Test Dictionary"));
                let entry = &entries[0];

                let Block::Paragraph(headword) = &entry.blocks[0] else {
                    panic!("expected headword paragraph");
                };
                assert_eq!(headword.style.size, Some(20.0));
                assert_eq!(
                    headword.style.color,
                    Some(crate::model::Color::rgb(0, 0, 255))
                );

                let text = entry.text();
                assert!(text.contains("世界"), "{text}");
                assert!(text.contains("※"), "{text}");
                assert!(text.contains("标记"), "{text}");
                assert!(entry
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        Block::Paragraph(paragraph) => Some(paragraph),
                        Block::Table(_) | Block::Container(_) => None,
                    })
                    .flat_map(|paragraph| paragraph.spans.iter())
                    .any(|span| matches!(span, Span::Text(text) if text.style.bold && text.text.contains("世界"))));
                assert!(
                    entry
                        .blocks
                        .iter()
                        .filter_map(|block| match block {
                            Block::Paragraph(paragraph) => Some(paragraph),
                            Block::Table(_) | Block::Container(_) => None,
                        })
                        .flat_map(|paragraph| paragraph.spans.iter())
                        .any(|span| matches!(span, Span::Image(image) if image.width == 2))
                );
            }
            _ => panic!("expected Found event"),
        }

        engine
            .commands
            .send(Command::Lookup {
                request: 8,
                term: "redirect".to_string(),
            })
            .unwrap();

        match futures::executor::block_on(rx.next()) {
            Some(Event::Found {
                request,
                entries,
                redirected,
                ..
            }) => {
                assert_eq!(request, 8);
                assert!(redirected);
                assert_eq!(entries[0].headword, "hello");
            }
            _ => panic!("expected redirected Found event"),
        }

        std::fs::remove_file(mdx_path).ok();
        std::fs::remove_file(mdd_path).ok();
    }

    fn write_simple_dictionary(stem: &str, title: &str, entries: &[(&str, &str)]) -> PathBuf {
        let mut builder =
            MdxBuilder::with_options(WriteOptions::new().with_encoding(WriteEncoding::Utf8));
        builder.header_attribute("Title", title).unwrap();
        for (headword, body) in entries {
            builder.add_entry(*headword, *body).unwrap();
        }
        let mut bytes = Vec::new();
        builder.finish(&mut bytes).unwrap();

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "gpui-pdf-{stem}-{}-{stamp}.mdx",
            std::process::id()
        ));
        std::fs::write(&path, &bytes).unwrap();
        path
    }

    #[test]
    fn worker_suggests_prefixes_across_dictionaries_with_dedupe_and_cap() {
        let first_entries: Vec<(String, String)> = (0..9)
            .map(|index| (format!("pre{index}"), format!("<div>甲{index}</div>")))
            .collect();
        let first: Vec<(&str, &str)> = first_entries
            .iter()
            .map(|(headword, body)| (headword.as_str(), body.as_str()))
            .collect();
        let first = write_simple_dictionary("prefix-a", "词典甲", &first);
        let second = write_simple_dictionary(
            "prefix-b",
            "词典乙",
            &[
                ("pre0", "<div>乙重复</div>"),
                ("prefix-two", "<div>乙</div>"),
            ],
        );

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let engine = DictEngine::spawn(tx);
        engine
            .commands
            .send(Command::Configure {
                request: 1,
                dictionaries: vec![
                    DictionaryConfig {
                        path: first.clone(),
                        enabled: true,
                    },
                    DictionaryConfig {
                        path: second.clone(),
                        enabled: true,
                    },
                ],
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::Configured { .. }) => {}
            _ => panic!("expected Configured event"),
        }

        engine
            .commands
            .send(Command::Prefix {
                request: 2,
                term: "pre".to_string(),
                limit: 20,
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::PrefixSuggestions {
                request,
                term,
                suggestions,
            }) => {
                assert_eq!(request, 2);
                assert_eq!(term, "pre");
                assert_eq!(suggestions.len(), 8, "跨词典合并后最多 8 条");
                let unique: HashSet<String> = suggestions
                    .iter()
                    .map(|value| value.to_lowercase())
                    .collect();
                assert_eq!(unique.len(), suggestions.len(), "建议按忽略大小写去重");
                assert_eq!(suggestions[0], "pre0");
            }
            _ => panic!("expected PrefixSuggestions event"),
        }

        // 精确前缀无结果时，去掉末尾字符得到近似前缀建议。
        engine
            .commands
            .send(Command::Prefix {
                request: 3,
                term: "prefix-twoo".to_string(),
                limit: 20,
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::PrefixSuggestions { suggestions, .. }) => {
                assert!(suggestions.contains(&"prefix-two".to_string()));
            }
            _ => panic!("expected approximate PrefixSuggestions event"),
        }

        for term in ["", "qqqq"] {
            engine
                .commands
                .send(Command::Prefix {
                    request: 4,
                    term: term.to_string(),
                    limit: 20,
                })
                .unwrap();
            match futures::executor::block_on(rx.next()) {
                Some(Event::PrefixSuggestions { suggestions, .. }) => {
                    assert!(suggestions.is_empty(), "{term:?} 不应有建议");
                }
                _ => panic!("expected empty PrefixSuggestions event"),
            }
        }

        std::fs::remove_file(first).ok();
        std::fs::remove_file(second).ok();
    }

    #[test]
    fn worker_aggregates_dictionaries_in_order_and_respects_enabled() {
        let first = write_simple_dictionary(
            "multi-a",
            "词典甲",
            &[("alpha", "<div>甲</div>"), ("shared", "<div>共享</div>")],
        );
        let second = write_simple_dictionary(
            "multi-b",
            "词典乙",
            &[("alpha", "<div>乙</div>"), ("beta", "<div>乙独有</div>")],
        );

        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let engine = DictEngine::spawn(tx);
        let configure = |request: u64, list: &[(PathBuf, bool)]| {
            engine
                .commands
                .send(Command::Configure {
                    request,
                    dictionaries: list
                        .iter()
                        .map(|(path, enabled)| DictionaryConfig {
                            path: path.clone(),
                            enabled: *enabled,
                        })
                        .collect(),
                })
                .unwrap();
        };

        configure(1, &[(first.clone(), true), (second.clone(), true)]);
        match futures::executor::block_on(rx.next()) {
            Some(Event::Configured { dictionaries, .. }) => {
                assert_eq!(dictionaries.len(), 2);
                assert_eq!(dictionaries[0].title.as_deref(), Some("词典甲"));
                assert_eq!(dictionaries[1].title.as_deref(), Some("词典乙"));
                assert!(dictionaries.iter().all(|info| info.enabled));
            }
            _ => panic!("expected Configured event"),
        }

        engine
            .commands
            .send(Command::Lookup {
                request: 10,
                term: "alpha".to_string(),
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::Found { entries, .. }) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].source.as_deref(), Some("词典甲"));
                assert_eq!(entries[1].source.as_deref(), Some("词典乙"));
            }
            _ => panic!("expected aggregated Found event"),
        }

        configure(2, &[(second.clone(), true), (first.clone(), true)]);
        match futures::executor::block_on(rx.next()) {
            Some(Event::Configured { dictionaries, .. }) => {
                assert_eq!(dictionaries[0].title.as_deref(), Some("词典乙"));
                assert_eq!(dictionaries[1].title.as_deref(), Some("词典甲"));
            }
            _ => panic!("expected Configured event"),
        }
        engine
            .commands
            .send(Command::Lookup {
                request: 11,
                term: "alpha".to_string(),
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::Found { entries, .. }) => {
                assert_eq!(entries[0].source.as_deref(), Some("词典乙"));
                assert_eq!(entries[1].source.as_deref(), Some("词典甲"));
            }
            _ => panic!("expected reordered Found event"),
        }

        configure(3, &[(first.clone(), false), (second.clone(), true)]);
        match futures::executor::block_on(rx.next()) {
            Some(Event::Configured { dictionaries, .. }) => {
                assert!(!dictionaries[0].enabled);
                assert!(dictionaries[1].enabled);
            }
            _ => panic!("expected Configured event"),
        }
        engine
            .commands
            .send(Command::Lookup {
                request: 12,
                term: "alpha".to_string(),
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::Found { entries, .. }) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].source.as_deref(), Some("词典乙"));
            }
            _ => panic!("expected single-dictionary Found event"),
        }

        engine
            .commands
            .send(Command::Lookup {
                request: 13,
                term: "shared".to_string(),
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::NotFound { .. }) => {}
            _ => panic!("disabled dictionary must not answer"),
        }

        configure(4, &[(first.clone(), true), (second.clone(), false)]);
        match futures::executor::block_on(rx.next()) {
            Some(Event::Configured { .. }) => {}
            _ => panic!("expected Configured event"),
        }
        engine
            .commands
            .send(Command::Lookup {
                request: 14,
                term: "shared".to_string(),
            })
            .unwrap();
        match futures::executor::block_on(rx.next()) {
            Some(Event::Found { entries, .. }) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].source.as_deref(), Some("词典甲"));
            }
            _ => panic!("expected Found event"),
        }

        std::fs::remove_file(first).ok();
        std::fs::remove_file(second).ok();
    }

    #[test]
    #[ignore = "需要设置 GPUI_PDF_TEST_DICT 指向《王力古漢語字典》的 .mdx"]
    fn wangli_dictionary_renders_styles_and_pseudo_content() {
        let path = std::env::var("GPUI_PDF_TEST_DICT").expect("GPUI_PDF_TEST_DICT");
        let (mut dictionary, _, _) =
            load_dictionary(&PathBuf::from(path)).expect("load dictionary");
        assert!(!dictionary.stylesheet.is_empty(), "external css not loaded");
        assert!(!dictionary.fonts.is_empty(), "font-face fonts not loaded");
        assert_eq!(
            dictionary.fonts.covers("WL-special", '～'),
            Some(true),
            "font cmap not parsed"
        );

        let (entry, _) = dictionary.lookup("禮").expect("lookup").expect("entry");
        let Block::Paragraph(headword) = &entry.blocks[0] else {
            panic!("expected headword paragraph");
        };
        assert!(
            headword.spans.iter().any(
                |span| matches!(span, Span::Text(text) if text.style.size.unwrap_or(0.0) > 20.0)
            ),
            "headword should use the 1.6em rule"
        );

        let text = entry.text();
        assert!(text.contains("～"), "missing w::before content: {text}");
        assert!(text.contains("❶"), "missing p[n]::before content: {text}");
        assert!(text.contains("頁"), "missing a[pg]::after content: {text}");
        assert!(text.contains("引"), "missing e[x]::before content: {text}");
        assert!(text.contains("836"), "missing page link: {text}");
        assert!(text.contains("盧啟切"), "missing pronunciation: {text}");
    }
}
