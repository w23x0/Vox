#!/usr/bin/env python3
"""往一个 X11 窗口发 `WM_DELETE_WINDOW` —— 等价于用户点窗口的关闭按钮。

为什么需要它：本机没有 `xdotool` / `wmctrl`，而 XWayland 下 `XTest` 是哑的
（`XTestFakeMotionEvent` 不动指针），`XSendEvent` 反而好使。用它验"关窗到底收进托盘
还是最小化"（`docs/platform/LINUX.md` §9.7）：

```bash
./target/debug/vox &
wid=$(xwininfo -root -tree | grep '"Vox"' | awk '{print $1}')
python3 tools/linux-verify/send_close.py "$wid"
xprop -id "$wid" WM_STATE      # Withdrawn = 收进托盘；Iconic = 最小化
```

注意 `grep '"Vox"'` 会同时匹配悬浮字幕窗（标题是 `Vox 字幕`）——那个窗
关掉不影响进程，要验主窗就按 `960x640` 的尺寸挑。
"""
import ctypes
import ctypes.util
import sys


class ClientMessageData(ctypes.Union):
    _fields_ = [("b", ctypes.c_char * 20), ("s", ctypes.c_short * 10), ("l", ctypes.c_long * 5)]


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", ctypes.c_ulong),
        ("message_type", ctypes.c_ulong),
        ("format", ctypes.c_int),
        ("data", ClientMessageData),
    ]


def main() -> int:
    window = int(sys.argv[1], 16)
    x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XInternAtom.restype = ctypes.c_ulong
    x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
    x11.XSendEvent.argtypes = [
        ctypes.c_void_p,
        ctypes.c_ulong,
        ctypes.c_int,
        ctypes.c_long,
        ctypes.POINTER(XClientMessageEvent),
    ]
    x11.XFlush.argtypes = [ctypes.c_void_p]

    dpy = x11.XOpenDisplay(None)
    if not dpy:
        print("no display", file=sys.stderr)
        return 1

    event = XClientMessageEvent()
    event.type = 33  # ClientMessage
    event.window = window
    event.message_type = x11.XInternAtom(dpy, b"WM_PROTOCOLS", False)
    event.format = 32
    event.data.l[0] = x11.XInternAtom(dpy, b"WM_DELETE_WINDOW", False)
    event.data.l[1] = 0  # CurrentTime
    sent = x11.XSendEvent(dpy, window, False, 0, ctypes.byref(event))
    x11.XFlush(dpy)
    print(f"sent WM_DELETE_WINDOW to 0x{window:x}: {bool(sent)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
