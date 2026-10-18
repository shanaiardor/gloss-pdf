//! 内嵌静态资源：图标直接编译进二进制，运行时不再依赖工作目录。

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(icon(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let names: Vec<SharedString> = match path.trim_matches('/') {
            "" => vec![SharedString::from("icons")],
            "icons" => ICON_PATHS
                .iter()
                .filter_map(|path| path.strip_prefix("icons/"))
                .map(SharedString::from)
                .collect(),
            _ => Vec::new(),
        };
        Ok(names)
    }
}

const ICON_PATHS: &[&str] = &[
    "icons/home.svg",
    "icons/open.svg",
    "icons/outline.svg",
    "icons/search.svg",
    "icons/highlight.svg",
    "icons/annotations.svg",
    "icons/dictionary.svg",
    "icons/save.svg",
    "icons/settings.svg",
];

fn icon(path: &str) -> Option<&'static [u8]> {
    Some(match path {
        "icons/home.svg" => include_bytes!("../assets/icons/home.svg"),
        "icons/open.svg" => include_bytes!("../assets/icons/open.svg"),
        "icons/outline.svg" => include_bytes!("../assets/icons/outline.svg"),
        "icons/search.svg" => include_bytes!("../assets/icons/search.svg"),
        "icons/highlight.svg" => include_bytes!("../assets/icons/highlight.svg"),
        "icons/annotations.svg" => include_bytes!("../assets/icons/annotations.svg"),
        "icons/dictionary.svg" => include_bytes!("../assets/icons/dictionary.svg"),
        "icons/save.svg" => include_bytes!("../assets/icons/save.svg"),
        "icons/settings.svg" => include_bytes!("../assets/icons/settings.svg"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_icon_is_embedded() {
        for path in ICON_PATHS {
            let asset = Assets.load(path).expect("load icon");

            assert!(asset.is_some(), "missing icon: {path}");
        }
    }

    #[test]
    fn unknown_assets_are_reported_as_missing() {
        assert_eq!(Assets.load("icons/nope.svg").unwrap(), None);
    }
}
