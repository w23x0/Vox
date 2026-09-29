#!/usr/bin/env python3
"""用 WebKitGTK 渲染前端并存成 PNG —— 证明"界面到底画出来没有"。

为什么需要它：`docs/platform/LINUX.md` 里那条"像素级确认"卡了很久。Linux 上是
GNOME Wayland，rootless XWayland 下 `ffmpeg -f x11grab` 抓根窗口只有黑屏（X root 上
没有合成结果），GNOME Shell 的 `org.gnome.Shell.Screenshot` 报 AccessDenied，portal
截图要人工点同意。而 WebKitGTK 自己就有 `webkit_web_view_get_snapshot`，能把 WebView
的真实渲染结果直接吐成图，不需要合成器配合。

它跑的是**同一个 WebKitGTK**（本机 2.52.6，跟 app 里那份一致），所以能回答那个真问题：
"NVIDIA + WebKitGTK 的 DMABUF 渲染器会不会把窗口画成黑屏"。

用法：

```bash
# 1. 起前端（带 mock 后端，不需要 Tauri IPC）
cd app/ui && npm run dev
# 2. 截图
GDK_BACKEND=x11 python3 tools/linux-verify/webkit_shot.py \\
    "http://127.0.0.1:5183/?mock=1" /tmp/ui.png
```

前置：`python3-gi`、`gir1.2-webkit2-4.1`、`python3-gi-cairo`（缺最后一个会在
`get_snapshot_finish` 上炸 "Couldn't find foreign struct converter for 'cairo.Surface'"）。

想抓**真 app 窗口**的像素（不是 WebKit 离屏渲染）用 ImageMagick：

```bash
./target/debug/vox &            # 会话是 Wayland 时它会自己切 GDK_BACKEND=x11
import -window "$(xwininfo -root -tree | grep '\"Vox\"' | awk '{print $1}')" /tmp/app.png
```
"""
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import GLib, Gtk, WebKit2  # noqa: E402

state = {"loaded": False, "done": False}


def main() -> int:
    url = sys.argv[1]
    out = sys.argv[2]

    win = Gtk.Window(title="WebKitProbe")
    win.set_default_size(1000, 700)
    view = WebKit2.WebView()
    win.add(view)
    win.show_all()

    def on_snapshot(_view, result) -> None:
        try:
            surface = view.get_snapshot_finish(result)
        except Exception as exc:  # noqa: BLE001
            print(f"snapshot failed: {exc}", flush=True)
            Gtk.main_quit()
            return
        surface.write_to_png(out)
        print(f"saved {out} {surface.get_width()}x{surface.get_height()}", flush=True)
        Gtk.main_quit()

    def snapshot() -> bool:
        if state["done"]:
            return False
        state["done"] = True
        view.get_snapshot(
            WebKit2.SnapshotRegion.FULL_DOCUMENT,
            WebKit2.SnapshotOptions.NONE,
            None,
            on_snapshot,
        )
        return False

    def on_load(_view, event) -> None:
        # 等 React 挂载 + mock 数据到位再截，否则可能截到空壳。
        if event == WebKit2.LoadEvent.FINISHED and not state["loaded"]:
            state["loaded"] = True
            GLib.timeout_add(2500, snapshot)

    view.connect("load-changed", on_load)
    view.load_uri(url)
    GLib.timeout_add(20000, lambda: (print("timeout", flush=True), Gtk.main_quit()))
    Gtk.main()
    return 0


if __name__ == "__main__":
    sys.exit(main())
