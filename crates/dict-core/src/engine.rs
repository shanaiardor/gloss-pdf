//! 词典 worker：加载 `.mdx`（及同名 `.mdd`、外置 CSS/字体）并响应查询。
//!
//! 每本 MDX 首次加载时会在它旁边生成 `<文件名>.mdx.keyidx` 持久化 key index，
//! 之后启动直接复用，避免每次进程第一次查词都重建约 94 万条词头的内存索引。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;
use mdictlib::{KeyEntry, KeyIndex, KeyIndexOptions, KeyIndexSourceIdentity, MddFile, MdxFile};

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
/// `@import` 的最大递归深度。
const MAX_CSS_DEPTH: usize = 8;
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
        let keys = match dictionary.dictionary.prefix_keys(term, per_dictionary) {
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
    /// 与 MDX 放在一起的持久化 key index；不可用时退回 mdictlib 的内存索引。
    key_index: Option<KeyIndex>,
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

        let Some((mut headword, mut body)) = find_entry(&self.mdx, self.key_index.as_ref(), term)?
        else {
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

            match find_entry(&self.mdx, self.key_index.as_ref(), &target)? {
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

    /// 前缀联想同样优先走持久化索引，避免第一次输入触发 150ms 级的内存索引构建。
    fn prefix_keys(&self, term: &str, limit: usize) -> Result<Vec<KeyEntry>> {
        Ok(match &self.key_index {
            Some(index) => self.mdx.prefix_keys_with_index(index, term, limit)?,
            None => self.mdx.prefix_keys(term, limit)?,
        })
    }
}

fn load_dictionary(path: &Path) -> Result<(Dictionary, Option<String>, u64)> {
    let mdx = MdxFile::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let key_index = load_key_index(&mdx, path);
    let title = mdx.header().title().map(|title| title.to_string());
    let entries = mdx.len();
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mdd = open_mdds(path);
    let (stylesheet, loaded_stylesheets) = load_stylesheet(&mdx, path, &base_dir, &mdd);
    let mut font_faces = stylesheet.font_faces().to_vec();
    let mut font_cache = HashMap::new();
    let mut font_css_bytes = 0usize;
    for href in discover_css_sources(&base_dir, &mdd) {
        let Some(css) = load_css_graph(&base_dir, &mdd, &href, &mut font_cache, &mut Vec::new(), 0)
        else {
            continue;
        };
        font_css_bytes += css.len();
        if font_css_bytes > MAX_CSS_BYTES {
            log::warn!("字体扫描的 CSS 预算耗尽");
            break;
        }
        font_faces.extend(Stylesheet::parse(&css).font_faces().iter().cloned());
    }
    let mut seen_faces = HashSet::new();
    font_faces.retain(|face| {
        seen_faces.insert(format!(
            "{}|{}",
            face.family.to_ascii_lowercase(),
            face.sources.join("|")
        ))
    });
    let fonts = load_fonts(&font_faces, &base_dir, &mdd);

    Ok((
        Dictionary {
            mdx,
            key_index,
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

/// 与 MDX 同目录的持久化 key index 路径：`foo.mdx` → `foo.mdx.keyidx`。
fn key_index_path(mdx_path: &Path) -> PathBuf {
    let mut name = mdx_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".keyidx");
    mdx_path.with_file_name(name)
}

/// 先写到同目录的临时文件，成功后再换名，避免留下半个索引。
fn key_index_temp_path(index_path: &Path) -> PathBuf {
    let mut name = index_path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    index_path.with_file_name(name)
}

/// 打开 MDX 旁边的持久化索引；缺失或失效时构建一次。
///
/// 返回 `None` 表示目录不可写、索引损坏且重建失败等；此时查询照常使用
/// mdictlib 的内存索引，只是第一次查词会慢一点。
fn load_key_index(mdx: &MdxFile, mdx_path: &Path) -> Option<KeyIndex> {
    let identity = match mdx.key_index_source_identity() {
        Ok(identity) => identity,
        Err(error) => {
            log::warn!(
                "无法读取 {} 的索引元数据，改用内存索引: {error:#}",
                mdx_path.display()
            );
            return None;
        }
    };

    let index_path = key_index_path(mdx_path);
    let options = KeyIndexOptions::new();
    if index_path.is_file() {
        match mdx.open_key_index(&index_path, &identity, &options) {
            Ok(index) => {
                log::info!(
                    "opened key index {} ({} keys)",
                    index_path.display(),
                    identity.key_count()
                );
                return Some(index);
            }
            Err(error) => log::info!(
                "key index {} 已过期或损坏，正在重建: {error:#}",
                index_path.display()
            ),
        }
    }

    match build_key_index(mdx, &identity, &index_path, &options) {
        Ok(index) => Some(index),
        Err(error) => {
            log::warn!(
                "无法为 {} 建立持久化索引，改用内存索引: {error:#}",
                mdx_path.display()
            );
            None
        }
    }
}

fn build_key_index(
    mdx: &MdxFile,
    identity: &KeyIndexSourceIdentity,
    index_path: &Path,
    options: &KeyIndexOptions,
) -> Result<KeyIndex> {
    let temp_path = key_index_temp_path(index_path);
    let _ = std::fs::remove_file(&temp_path);
    let started = std::time::Instant::now();

    let result = (|| -> Result<KeyIndex> {
        let build = mdx
            .build_key_index_to_path(&temp_path, options, || false)
            .with_context(|| format!("创建索引 {}", temp_path.display()))?;
        publish_key_index(&temp_path, index_path)
            .with_context(|| format!("写入索引 {}", index_path.display()))?;
        log::info!(
            "built key index {} ({} keys, {} bytes, {:?})",
            index_path.display(),
            identity.key_count(),
            build.bytes_written(),
            started.elapsed()
        );
        mdx.open_key_index(index_path, identity, options)
            .with_context(|| format!("打开新索引 {}", index_path.display()))
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn publish_key_index(temp_path: &Path, index_path: &Path) -> std::io::Result<()> {
    // Windows 的 rename 不会覆盖已有文件；索引只是缓存，先删旧文件再换名。
    if index_path.exists() {
        std::fs::remove_file(index_path)?;
    }
    std::fs::rename(temp_path, index_path)
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

/// 词典级样式：优先加载文档实际引用的 CSS，只有完全没引用时才回退到扫描。
fn load_stylesheet(
    mdx: &MdxFile,
    path: &Path,
    base_dir: &Path,
    mdds: &[MddFile],
) -> (Stylesheet, Vec<String>) {
    let mut stylesheet = Stylesheet::default();
    let mut loaded = Vec::new();
    let mut cache = HashMap::new();
    let mut seen = HashSet::new();

    let description = mdx.header().description().unwrap_or_default();
    for href in css_links(description) {
        if !seen.insert(normalize_key(&href)) {
            continue;
        }
        match load_css_graph(base_dir, mdds, &href, &mut cache, &mut Vec::new(), 0) {
            Some(css) => {
                if css.len() > MAX_CSS_BYTES {
                    log::warn!("样式表超过预算，跳过 {href}");
                    continue;
                }
                stylesheet.extend(Stylesheet::parse(&css));
                loaded.push(normalize_key(&href));
            }
            None => log::warn!("找不到词典样式表 {href}"),
        }
    }

    for (name, value) in mdx.header().attributes() {
        if name.eq_ignore_ascii_case("stylesheet") && !value.trim().is_empty() {
            let css = rewrite_css_urls(value, "");
            stylesheet.extend(Stylesheet::parse(&css));
        }
    }

    if stylesheet.is_empty()
        && let Some((href, css)) = fallback_stylesheet(path, base_dir, mdds, &mut cache)
    {
        stylesheet.extend(Stylesheet::parse(&css));
        loaded.push(normalize_key(&href));
    }

    let stats = stylesheet.stats();
    log::info!(
        "stylesheet: {} rules ({} source), at-rules {:?}, unsupported declarations {}, unsupported selectors {}",
        stats.compiled_rules,
        stats.style_rules,
        stats.at_rules,
        stats.unsupported_declarations_total(),
        stats.unsupported_selectors
    );
    if !stats.unsupported_declarations.is_empty() {
        log::debug!(
            "unsupported declarations: {:?}",
            stats.unsupported_declarations
        );
    }

    (stylesheet, loaded)
}

fn fallback_stylesheet(
    mdx_path: &Path,
    base_dir: &Path,
    mdds: &[MddFile],
    cache: &mut HashMap<String, Option<String>>,
) -> Option<(String, String)> {
    let stem = mdx_path
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())?;
    let mut candidates = vec![
        format!("{stem}.css"),
        "style.css".to_string(),
        "default.css".to_string(),
    ];
    let mut from_mdd = Vec::new();
    for mdd in mdds {
        for key in mdd.keys().flatten() {
            let name = key.key().to_string();
            if !name.to_ascii_lowercase().ends_with(".css") {
                continue;
            }
            if let Some(normalized) = normalized_resource(&name)
                && !from_mdd.contains(&normalized)
            {
                from_mdd.push(normalized);
            }
            if from_mdd.len() >= MAX_CSS_FILES {
                break;
            }
        }
    }
    from_mdd.sort();
    candidates.extend(from_mdd);

    for href in candidates {
        if let Some(css) = load_css_graph(base_dir, mdds, &href, cache, &mut Vec::new(), 0) {
            return Some((href, css));
        }
    }
    None
}

fn load_css_graph(
    base_dir: &Path,
    mdds: &[MddFile],
    href: &str,
    cache: &mut HashMap<String, Option<String>>,
    stack: &mut Vec<String>,
    depth: usize,
) -> Option<String> {
    if depth > MAX_CSS_DEPTH {
        return None;
    }
    let key = normalize_key(href);
    if key.is_empty() {
        return None;
    }
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    if stack.iter().any(|value| value == &key) {
        log::warn!("CSS @import 循环: {href}");
        return None;
    }

    stack.push(key.clone());
    let result = read_css_source(base_dir, mdds, href).and_then(|(bytes, dir)| {
        let css = decode_text(&bytes);
        expand_css_imports(&css, &dir, base_dir, mdds, cache, stack, depth)
    });
    stack.pop();
    cache.insert(key, result.clone());
    result
}

fn read_css_source(base_dir: &Path, mdds: &[MddFile], href: &str) -> Option<(Vec<u8>, String)> {
    let normalized = normalized_resource(href)?;
    if let Some(path) = read_external(base_dir, href) {
        let bytes = std::fs::read(path).ok()?;
        return Some((bytes, parent_dir(&normalized)));
    }

    let (bytes, key) = lookup_mdd_resource_entry(mdds, href)?;
    let key = normalized_resource(&key)?;
    Some((bytes.to_vec(), parent_dir(&key)))
}

fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

fn expand_css_imports(
    css: &str,
    dir: &str,
    base_dir: &Path,
    mdds: &[MddFile],
    cache: &mut HashMap<String, Option<String>>,
    stack: &mut Vec<String>,
    depth: usize,
) -> Option<String> {
    let lower = css.to_ascii_lowercase();
    let mut output = String::with_capacity(css.len());
    let mut last = 0;
    let mut search = 0;

    while let Some(relative) = lower[search..].find("@import") {
        let at = search + relative;
        let Some(end) = find_statement_end(css, at) else {
            break;
        };
        output.push_str(&rewrite_css_urls(&css[last..at], dir));

        let statement = &css[at..end];
        if let Some((href, supports, media)) = parse_import_statement(statement)
            && let Some(target) = join_href(dir, &href)
            && let Some(imported) = load_css_graph(base_dir, mdds, &target, cache, stack, depth + 1)
        {
            let mut wrapped = imported;
            if let Some(supports) = supports
                && !supports.is_empty()
            {
                wrapped = format!("@supports {supports} {{ {wrapped} }}");
            }
            if let Some(media) = media
                && !media.is_empty()
            {
                wrapped = format!("@media {media} {{ {wrapped} }}");
            }
            output.push_str(&wrapped);
        } else {
            log::warn!("跳过无法加载的 @import: {statement}");
        }

        last = end + 1;
        search = last;
    }

    output.push_str(&rewrite_css_urls(&css[last..], dir));
    Some(output)
}

/// 从 `@import` 起找到语句结尾的 `;`（跳过字符串和括号）。
fn find_statement_end(css: &str, start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for (offset, ch) in css[start..].char_indices() {
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '(' => depth += 1,
            ')' => depth -= 1,
            ';' if depth == 0 => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn parse_import_statement(statement: &str) -> Option<(String, Option<String>, Option<String>)> {
    let rest = statement.trim();
    if rest.len() < 7 || !rest[..7].eq_ignore_ascii_case("@import") {
        return None;
    }
    let rest = rest[7..].trim_start();

    let (href, rest) = if let Some(rest) = rest.strip_prefix("url(") {
        let end = rest.find(')')?;
        (
            rest[..end].trim().trim_matches(['"', '\'']).to_string(),
            rest[end + 1..].trim(),
        )
    } else {
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let rest = &rest[1..];
        let end = rest.find(quote)?;
        (rest[..end].to_string(), rest[end + 1..].trim())
    };

    let mut rest = rest.to_string();
    let mut supports = None;
    if let Some(index) = rest.find("supports(") {
        let start = index + "supports(".len();
        if let Some(end) = find_matching_paren(&rest, start) {
            supports = Some(rest[start..end].trim().to_string());
            rest.replace_range(index..=end, "");
        }
    }
    if let Some(index) = rest.find("layer") {
        let after = index + "layer".len();
        if let Some(rest_after) = rest[after..].strip_prefix('(') {
            if let Some(end) = rest_after.find(')') {
                let end = after + 1 + end + 1;
                rest.replace_range(index..end, "");
            }
        } else {
            rest.replace_range(index..after, "");
        }
    }
    let media = rest.trim();
    let media = (!media.is_empty()).then(|| media.to_string());

    Some((href, supports, media))
}

fn join_href(dir: &str, href: &str) -> Option<String> {
    normalize_join(dir, href)
}

fn normalize_join(dir: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("data:")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
        || lower.starts_with("//")
        || lower.starts_with('#')
        || lower.starts_with("javascript:")
    {
        return None;
    }

    let decoded = percent_decode(href);
    let decoded = decoded.split(['?', '#']).next().unwrap_or(&decoded).trim();
    let mut parts: Vec<&str> = if decoded.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|part| !part.is_empty()).collect()
    };
    let normalized = decoded.replace('\\', "/");
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    (!joined.is_empty()).then_some(joined)
}

fn rewrite_css_urls(css: &str, dir: &str) -> String {
    let lower = css.to_ascii_lowercase();
    let mut output = String::with_capacity(css.len());
    let mut cursor = 0;

    while let Some(relative) = lower[cursor..].find("url(") {
        let start = cursor + relative;
        output.push_str(&css[cursor..start]);
        let inner_start = start + 4;
        let Some(close) = find_matching_paren(css, inner_start) else {
            output.push_str(&css[start..]);
            return output;
        };
        let inner = css[inner_start..close].trim().trim_matches(['"', '\'']);
        match normalize_join(dir, inner) {
            Some(joined) => output.push_str(&format!("url(\"{joined}\")")),
            None => output.push_str(&css[start..=close]),
        }
        cursor = close + 1;
    }

    output.push_str(&css[cursor..]);
    output
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

/// 只用于字体发现：列出词典目录和 MDD 里的 CSS，不合并其中的规则。
fn discover_css_sources(base_dir: &Path, mdds: &[MddFile]) -> Vec<String> {
    let mut sources = Vec::new();
    let mut seen = HashSet::new();

    if let Ok(entries) = std::fs::read_dir(base_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("css"))
                && let Some(name) = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                && seen.insert(name.to_ascii_lowercase())
            {
                sources.push(name);
            }
            if sources.len() >= MAX_CSS_FILES {
                return sources;
            }
        }
    }

    for mdd in mdds {
        for key in mdd.keys().flatten() {
            let name = key.key();
            if !name.to_ascii_lowercase().ends_with(".css") {
                continue;
            }
            if let Some(normalized) = normalized_resource(name)
                && seen.insert(normalized.to_ascii_lowercase())
            {
                sources.push(normalized);
            }
            if sources.len() >= MAX_CSS_FILES {
                return sources;
            }
        }
    }

    sources
}

fn load_fonts(faces: &[crate::css::FontFace], base_dir: &Path, mdds: &[MddFile]) -> FontLibrary {
    let mut library = FontLibrary::new();
    let mut total = 0;
    for face in faces {
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
    if library.is_empty() && !faces.is_empty() {
        log::warn!(
            "词典声明了 {} 个 @font-face，但没有加载到可用字体",
            faces.len()
        );
    }
    library
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
    lookup_mdd_resource_entry(mdds, src).map(|(bytes, _)| bytes)
}

fn lookup_mdd_resource_entry(mdds: &[MddFile], src: &str) -> Option<(Arc<[u8]>, String)> {
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
                return Some((
                    Arc::from(resource.bytes().to_vec()),
                    resource.key().to_string(),
                ));
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
        if self.stylesheets.len() >= MAX_CACHE_ENTRIES {
            self.stylesheets.clear();
        }
        let MddResolver {
            base_dir,
            mdd,
            stylesheets,
            ..
        } = self;
        let value = load_css_graph(base_dir, mdd, href, stylesheets, &mut Vec::new(), 0);
        if let Some(css) = &value {
            log::info!("entry stylesheet: {href} ({} bytes)", css.len());
        }
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

/// CSS 文件可能是 UTF-8（带/不带 BOM）、UTF-16，或旧词典常见的 GBK/GB18030。
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
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }

    let (text, _, had_errors) = encoding_rs::GB18030.decode(bytes);
    if !had_errors {
        return text.into_owned();
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

fn find_entry(
    dictionary: &MdxFile,
    key_index: Option<&KeyIndex>,
    term: &str,
) -> Result<Option<(String, String)>> {
    for candidate in candidates(term) {
        let entry = match key_index {
            Some(index) => match dictionary
                .locate_with_key_index(index, &candidate)
                .with_context(|| format!("查询 {candidate:?} 失败"))?
            {
                Some(matches) => dictionary
                    .entry_at(matches.first())
                    .with_context(|| format!("读取 {candidate:?} 的词条失败"))?,
                None => None,
            },
            None => dictionary
                .lookup(&candidate)
                .with_context(|| format!("查询 {candidate:?} 失败"))?,
        };
        if let Some(entry) = entry {
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

    /// 删除测试词典以及它旁边的持久化索引，避免临时目录里越积越多。
    fn remove_dictionary(path: &Path) {
        std::fs::remove_file(path).ok();
        std::fs::remove_file(key_index_path(path)).ok();
    }

    #[test]
    fn key_index_path_sits_next_to_the_mdx() {
        assert_eq!(
            key_index_path(Path::new("dicts/foo.mdx")),
            PathBuf::from("dicts/foo.mdx.keyidx")
        );
    }

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
    fn css_urls_are_rewritten_relative_to_their_stylesheet() {
        let css = rewrite_css_urls(
            r#".a { background: url("../img/a.png"); }
               .b { background: url(data:image/png;base64,AAAA); }"#,
            "css/sub",
        );
        assert!(css.contains(r#"url("css/img/a.png")"#), "{css}");
        assert!(css.contains("url(data:image/png;base64,AAAA)"), "{css}");
        assert_eq!(
            normalize_join("css", "../fonts/a.woff").as_deref(),
            Some("fonts/a.woff")
        );
        assert_eq!(
            normalize_join("css", "/img/a.png").as_deref(),
            Some("img/a.png")
        );
    }

    #[test]
    fn css_import_statements_keep_media_and_supports() {
        let (href, supports, media) =
            parse_import_statement("@import url(\"theme.css\") screen and (min-width: 400px)")
                .unwrap();
        assert_eq!(href, "theme.css");
        assert!(supports.is_none());
        assert_eq!(media.as_deref(), Some("screen and (min-width: 400px)"));

        let (href, supports, media) =
            parse_import_statement("@import 'print.css' supports(display: grid) print").unwrap();
        assert_eq!(href, "print.css");
        assert_eq!(supports.as_deref(), Some("display: grid"));
        assert_eq!(media.as_deref(), Some("print"));
    }

    #[test]
    fn external_import_graph_is_expanded() {
        let directory = std::env::temp_dir().join(format!(
            "gloss-pdf-css-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let css_dir = directory.join("css");
        std::fs::create_dir_all(&css_dir).unwrap();
        std::fs::write(
            css_dir.join("main.css"),
            r#"@import "theme.css";
               .a { background-image: url(../img/a.png); }"#,
        )
        .unwrap();
        std::fs::write(css_dir.join("theme.css"), "html { --brand: #123456; }").unwrap();

        let mut cache = HashMap::new();
        let css = load_css_graph(
            &directory,
            &[],
            "css/main.css",
            &mut cache,
            &mut Vec::new(),
            0,
        )
        .expect("css graph");
        assert!(css.contains("--brand"), "{css}");
        assert!(css.contains(r#"url("img/a.png")"#), "{css}");
        assert!(!css.contains("@import"), "{css}");

        std::fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn gb18030_css_is_decoded() {
        // “你好”的 GBK 编码。
        assert_eq!(decode_text(&[0xC4, 0xE3, 0xBA, 0xC3]), "你好");
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
                r#"<link rel="stylesheet" href="css/main.css">
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
        let stem = format!("gloss-pdf-dict-test-{}-{stamp}", std::process::id());
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
            "/css/main.css",
            r#"@import "theme.css";
               .hwd { font-size: 20px; color: var(--brand); }
               .def { color: #333333; background-image: url(../img/dot.png); }
               mark::before { content: "※"; }"#
                .as_bytes(),
        )
        .unwrap();
        mdd.add_resource("/css/theme.css", r#"html { --brand: #0000ff; }"#.as_bytes())
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
                assert!(
                    key_index_path(&mdx_path).is_file(),
                    "persistent key index should be written next to the mdx"
                );
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

        remove_dictionary(&mdx_path);
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
            "gloss-pdf-{stem}-{}-{stamp}.mdx",
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

        remove_dictionary(&first);
        remove_dictionary(&second);
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

        remove_dictionary(&first);
        remove_dictionary(&second);
    }

    #[test]
    #[ignore = "需要 GLOSS_PDF_TEST_DICT_ENTRY 指向一个把 CSS 放在词条 <link> 里的 MDX"]
    fn entry_level_stylesheet_is_applied() {
        let path = std::env::var("GLOSS_PDF_TEST_DICT_ENTRY").expect("GLOSS_PDF_TEST_DICT_ENTRY");
        let (mut dictionary, _, _) =
            load_dictionary(&PathBuf::from(path)).expect("load dictionary");
        let key = dictionary
            .mdx
            .keys()
            .flatten()
            .next()
            .expect("first key")
            .key()
            .to_string();
        let (entry, _) = dictionary
            .lookup(&key)
            .expect("lookup")
            .expect("entry body");

        let styled = entry.blocks.iter().any(|block| match block {
            Block::Paragraph(paragraph) => paragraph.spans.iter().any(|span| {
                matches!(span, Span::Text(text) if text.style.size.is_some() || text.style.color.is_some())
            }),
            Block::Container(container) => container.style.size.is_some(),
            Block::Table(table) => table.style.size.is_some(),
        });
        assert!(styled, "词条 {key:?} 没有应用词条内 <link> 的样式");
        let has_inline_box = entry.blocks.iter().any(|block| match block {
            Block::Paragraph(paragraph) => paragraph
                .spans
                .iter()
                .any(|span| matches!(span, Span::Box(_))),
            Block::Container(container) => container.items.iter().any(|item| {
                item.blocks.iter().any(|block| match block {
                    Block::Paragraph(paragraph) => paragraph
                        .spans
                        .iter()
                        .any(|span| matches!(span, Span::Box(_))),
                    _ => false,
                })
            }),
            Block::Table(_) => false,
        });
        assert!(has_inline_box, "词条 {key:?} 没有生成 inline-block 盒子");
    }

    #[test]
    #[ignore = "需要设置 GLOSS_PDF_TEST_DICT 指向《王力古漢語字典》的 .mdx"]
    fn wangli_dictionary_renders_styles_and_pseudo_content() {
        let path = std::env::var("GLOSS_PDF_TEST_DICT").expect("GLOSS_PDF_TEST_DICT");
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
        let headword_large = headword.style.size.unwrap_or(0.0) > 20.0
            || headword.spans.iter().any(|span| match span {
                Span::Text(text) => text.style.size.unwrap_or(0.0) > 20.0,
                // `.hw` 是 inline-block，字号落在盒子样式上。
                Span::Box(inline) => inline.style.size.unwrap_or(0.0) > 20.0,
                Span::Image(_) => false,
            });
        assert!(headword_large, "headword should use the 1.6em rule");

        let text = entry.text();
        assert!(text.contains("～"), "missing w::before content: {text}");
        assert!(text.contains("❶"), "missing p[n]::before content: {text}");
        assert!(text.contains("頁"), "missing a[pg]::after content: {text}");
        assert!(text.contains("引"), "missing e[x]::before content: {text}");
        assert!(text.contains("836"), "missing page link: {text}");
        assert!(text.contains("盧啟切"), "missing pronunciation: {text}");
    }
}
