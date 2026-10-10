# 悬浮字幕支持复杂文字（OV-2）

状态：已实现。接 `OVERLAY-CAPTIONS.md` 的 C11 口子（`layout.rs::TrackText` 的 `chars/advances/breaks` 并行数组）。

## 1. 选型

| 项 | 选择 | 理由 |
| --- | --- | --- |
| 整形 | `rustybuzz` 0.20（HarfBuzz 的纯 Rust 移植），放在 `vox-overlay-core::shape` | `swash` 的 shaper 只在 Linux crate 里，Windows 用不上；rustybuzz 在 core 里，两个平台吃同一份字体字节、得到同一组字形 id。 |
| 双向 | `unicode-bidi`（UAX #9） | 标准实现；基础方向用 `get_base_direction`（第一个强方向字符）。 |
| 脚本切分 | `unicode-script` | HarfBuzz 要求一段一个脚本。 |
| 断行 | 仍是 `unicode-linebreak`（UAX #14） | `fill_breaks` 仍是字节位置换算成单元下标的唯一处，只是改成按**簇起点字节**换算，簇内部的断点忽略。 |

## 2. 数据流

文字（逻辑顺序，控制字符换成空格）→ 基础方向（第一个强方向字符；锚点往前推过之后**钉死**，否则丢掉开头后整条字幕会翻面）
→ `BidiInfo` 分层，按层级切成同方向段 → 每段 `GlyphSource::shape` →（`FaceSet`：逐字选字体、按脚本切、rustybuzz 整形，簇一律按**逻辑顺序**输出）
→ 排版单元 = 字形簇：`chars`（簇首字符）/`advances`/`breaks`/`unit_chars`（簇→字符下标）/`levels`
→ `wrap` 不动（逻辑顺序贪心折行）→ 行定下来后 `visual_order`（UAX #9 的 L1 行尾空白 + L2 逐层反转）重排成显示顺序 → 按字形 id 画。

整形结果按"文字没变就复用"缓存在 `TrackScratch`，动画帧不重整形、不分配。

## 3. RTL 与不变量（S3）

采用方案 (a)：整条字幕基础方向为 RTL 时**右对齐**，所有视觉行共用右边缘，底衬贴合各行文字，新字长在左边。逻辑上靠前的簇距右边缘的距离只取决于它之前的簇，所以已显示的字（以簇的位置计）不动，I1 在逻辑顺序上成立；I3 对 RTL 轨改为"右边缘相同"。

固有例外（双向文字的本质，写进性质测试的豁免）：
- 行尾**反向的一段**（RTL 句子里的拉丁词/数字，LTR 句子里的阿拉伯词）在增长时，这一段自己的老字会被推开；
- 同类整词跳行时，原来那一行里的反向块一起重排。
单向文字（纯 RTL、纯 LTR、组合字形）严格无豁免，只留原有"整词跳下一行一次"。
连写字母追加时，只有**最后一个簇**的字形/宽度会变（词尾形→词中形），它之前的簇起点不变。
锚点落在行首（空格之后），连写/字距的上下文在此断开，重新整形不改变可见字；仅"一个超长词被迫按字断"时才可能有几像素差异。

## 4. 平台差异

- **Linux**：`SwashFont` 持有 `FaceSet`，`swash` 按 `(字体槽位, 字形 id)` 出掩码。字距 101.5% 取消（会拆开阿拉伯连写）。**回退**：主字体缺字时调 `fc-list :charset=<码点>`，在覆盖该字的字体面里偏好 无衬线/常规/非等宽/Noto；没有 `fc-list` 或没有覆盖的字体就留在主字体，画 `.notdef` 豆腐块，不 panic。
- **Windows**：`GetFontData` 读出 GDI 选中字体的整份文件（先试 `ttcf`，字体集合用 `pick_collection_face` 按族名+字重认出面），交给同一个 `FaceSet`；`ExtTextOutW` + `ETO_GLYPH_INDEX` 按字形 id 画（GDI 字形索引 = 字体 glyph id）。回退走固定候选表（Segoe UI / Nirmala UI / Leelawadee UI / YaHei UI / Yu Gothic UI / Malgun Gothic / Segoe UI Symbol / Arial Unicode MS），逐个 `CreateFontIndirectW`、核对 `GetTextFaceW`、读文件、验证覆盖。
  **未在真实 Windows 上跑过**（只做了 `cargo check --target x86_64-pc-windows-gnu --all-targets`）；GetFontData 取不到文件（非 TrueType/OpenType 字体）时 `FontRaster::new` 返回错误而不是静默画错。

## 5. 测试

- core：`visual_order`（L1/L2）、RTL 右边缘与底衬、簇→字符区间换算、`fill_breaks` 忽略簇内断点、基础方向与钉死、控制字符；性质测试 `invariants.rs` 分四种文字（拉丁中文 / 纯 RTL / 混排 / 附标）各 2000 条序列，附标不得落在行首。
- Linux（真字体）：`tests/shaping.rs`——阿拉伯连写（词首/中/尾字形不同、لا 一个簇）、RTL 右对齐与顺序、混排视觉顺序、泰语/印地语簇不拆、回退字体、缺字豆腐块。缺对应字体时该条打印原因后跳过。
- 快照：`cargo run -p vox-overlay-linux --example snapshot shaping`。
