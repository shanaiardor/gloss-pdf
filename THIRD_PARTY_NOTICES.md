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

## pdfium-render

- 许可：MIT OR Apache-2.0
- 上游：<https://github.com/ajrcarey/pdfium-render>

## GPUI / gpui_platform

- 许可：Apache-2.0
- 上游：<https://github.com/zed-industries/zed/tree/main/crates/gpui>

## 其他 Rust crate

其余依赖均为 crates.io 上的开源 crate，具体版本见 `Cargo.lock`。
需要生成完整清单时可以使用 `cargo install cargo-license && cargo license`。
