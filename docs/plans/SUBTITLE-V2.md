# 字幕设置改版（SUBTITLE-V2）

状态：设计稿（2026-10-10）。起因：Linux 字幕窗永久鼠标穿透（`DECISIONS.md` A5），位置只能「重置默认」，
`geometry` 从未被写过；设置页 13 项平铺；字体下拉是写死的 Windows 字体名。

## 1. 位置：「调整位置」模式（**已作废**：GNOME/XWayland 上拖不动，改为设置页内定位，见下）

> 2026-10-10 变更：鼠标拖动的编辑模式已整套移除。改为设置页「位置」组里的屏幕示意图（拖动 / 边缘缩放 / 方向键）、
> 九宫格预设与 X/Y/宽/高数字框，改 `subtitle.geometry`，Linux 悬浮窗经 `restyle` 实时搬窗（`Overlay::set_geometry`）。
> 下面是原设计，仅留档。

**原则**：平时永远穿透（A5 不动），只在用户主动进入「调整位置」时临时可交互。

流程：

1. 设置页「字幕 → 位置」点【调整位置】→ 后端 `overlay_set_edit_mode(true)`。
2. 字幕窗：
   - 取消输入穿透（input shape 还原为整窗）；
   - 画一圈 2px 高亮边框 + 角上四个缩放手柄 + 中间提示「拖动移动 · 拖边缘缩放 · 按 Esc / 点完成」；
   - 没有真实字幕时画两行示例文字（用设置里的字体/颜色），否则空窗没东西可拖；
   - 窗口临时抢焦点不要紧（编辑模式下去掉 `set_focus_on_map(false)` 的约束，或保持不抢焦点但用 `begin_move_drag`）。
3. 交互（GTK，主线程）：
   - 按下在窗体内部 → `window.begin_move_drag(button, root_x, root_y, time)`；
   - 按下在边缘 12px 内 → `window.begin_resize_drag(edge, …)`，边缘判定与 Windows `WM_NCHITTEST` 同一套（八向）；
   - 拖完（`configure-event` 去抖 200ms，或 `button-release`）读 `window.position()` / `size()`，
     经 `GeometryCallback` 写回 `subtitle.geometry`（与 Windows 同一路径，`app/src-tauri/src/overlay.rs`）。
4. 退出：设置页【完成】、窗口上按 Esc、或主窗口失焦 30 秒无操作 → `overlay_set_edit_mode(false)`，
   还原穿透，丢掉示例文字。**进程崩溃时不会留下可交互状态**（状态不落盘）。

**保底**（拖不动的合成器 / 想精确摆）：位置区再提供
- 预设锚点：屏幕 上/中/下 × 左/中/右 九宫格（写入 `geometry` 的 x/y，按当前显示器计算）；
- 「重置默认」保留。

XWayland 下 `move()` 与 `begin_move_drag` 都生效（`LINUX.md` §2.3）；纯 Wayland 不支持，沿用现有的「强制 x11」路线。

多显示器：保存 x/y 为虚拟桌面坐标；启动时若该点不在任何显示器内则回落默认位置（Windows 已有同款
`saved_geometry_far_offscreen_falls_back`，Linux 补同样的判断）。

## 2. 设置页分组

现状 13 项平铺。改为三组，其中「高级」默认折叠：

| 组 | 项 |
|---|---|
| 外观 | 显示开关、字体、字号、对外颜色、听人颜色、底衬透明度 |
| 显示行为 | 每轨行数、停留时间（char_ttl）、淡出时间（char_fade） |
| 位置 | 【调整位置】/【完成】、九宫格预设、重置默认 |
| 高级（折叠） | 淡化噪声字（dim_zeros）及透明度（dim_alpha）、VR 悬浮字幕 |

另外预览区（`SubtitlePreview`）保留在最上方。设置的字段/语义不变，只动排版，所以存档兼容，无迁移。

## 3. 字体列表

- Linux：下拉框的选项来自系统已装字体族（`fontdb` 枚举，去重排序，CJK 字体排前），
  另有「系统默认（无衬线）」一项；当前设置值若不在列表里仍钉在最前（现有行为）。
  Tauri 命令 `list_system_fonts`，Windows 侧返回现有写死列表或 GDI 枚举（可后做）。
- 默认字体按平台：Linux 默认 `""` → 走回退链（Noto Sans CJK），不再默认 `Microsoft YaHei UI`。
- 名字对不上时按「去修饰词词集」再匹配（已落地，`19061d2`）。

## 4. 工单

| 编号 | 内容 | 主要文件 |
|---|---|---|
| SUB-1 | Linux 编辑模式：输入穿透开关、边框/手柄/示例文字、拖动缩放、geometry 回写、Esc/超时退出、离屏回落；Tauri 命令 `overlay_set_edit_mode`；设置页【调整位置】按钮最小接线 | `vox-overlay-linux/src/window.rs`、`app/src-tauri/src/{overlay,commands}.rs`、`platform/linux/mod.rs` |
| SUB-2 | 设置页三组排版 + 高级折叠 + 九宫格预设 | `app/ui/src/sections/Subtitle.tsx`、i18n 三语 |
| SUB-3 | `list_system_fonts` 命令 + 下拉改用它 + Linux 默认字体 | `vox-overlay-linux/src/font.rs`、`app/src-tauri/src/commands.rs`、`Subtitle.tsx` |

SUB-2/3 都改 `Subtitle.tsx`，串行：先 SUB-1，再 SUB-2，最后 SUB-3。

## 5. 不做

- 不改 A5：平时仍永久穿透。
- 不改 `geometry` 数据形状（`x,y,width,height`）。
- 不碰 Windows 的拖动行为。
