# Third-party notices

`gpui-pdf` 本身以 MIT OR Apache-2.0 发布（见 `LICENSE-MIT` / `LICENSE-APACHE`）。
仓库同时包含以下第三方代码/二进制。

## PDFium（`pdfium.dll`）

- 版本：PDFium `157.0.8100.0`
- 构建来源：<https://github.com/bblanchon/pdfium-binaries>（该构建包的打包许可在 `third_party/pdfium/LICENSE`）
- 许可：BSD-3-Clause，版权归 The PDFium Authors
- PDFium 还包含第三方组件（freetype、harfbuzz、icu、libjpeg-turbo、libpng、lcms、openjpeg、zlib、abseil、simdutf 等），
  各自的许可文本在 `third_party/pdfium/licenses/` 下。
- 上游：<https://pdfium.googlesource.com/pdfium/>

分发本项目的二进制时，请保留 `pdfium.dll` 同目录的许可说明，或在“关于/第三方许可”中展示上述许可文本。

## LXGW 文楷 TC / LXGW WenKai TC（界面字体）

- 版本：`1.522`（2026-03-17）
- 文件：`crates/gpui-pdf/assets/fonts/lxgw-wenkai-tc/LXGWWenKaiTC-Regular.ttf`、
  `LXGWWenKaiMonoTC-Regular.ttf`
- 版权：Copyright 2022-2026 The LXGW WenKai Project Authors
  （<https://github.com/lxgw/LxgwWenkaiTC>，字形源自 The Klee Project Authors）
- 许可：SIL Open Font License 1.1，完整文本见字体同目录的 `OFL.txt`
- 上游：<https://github.com/lxgw/LxgwWenkaiTC>

字体数据编译进 `gpui-pdf` 二进制。按 OFL 第 2 条，分发二进制时需要一并提供
版权声明与许可文本：发布包里请保留 `crates/gpui-pdf/assets/fonts/lxgw-wenkai-tc/OFL.txt`
和本文件。

## pdfium-render

- 许可：MIT OR Apache-2.0
- 上游：<https://github.com/ajrcarey/pdfium-render>

## GPUI / gpui_platform

- 许可：Apache-2.0
- 上游：<https://github.com/zed-industries/zed/tree/main/crates/gpui>

## self_update（应用内更新）

- 版本：`1.3`
- 许可：MIT
- 上游：<https://github.com/jaemk/self_update>
- 说明：通过 GitHub Releases 检查版本并下载安装包；HTTP 由 `ureq` + `rustls`
  （均为 MIT OR Apache-2.0 / ISC 等宽松许可）完成。

## 其他 Rust crate

其余依赖均为 crates.io 上的开源 crate，具体版本见 `Cargo.lock`。
需要生成完整清单时可以使用 `cargo install cargo-license && cargo license`。
