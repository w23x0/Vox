//! 跟操作系统直接打交道的小件：时钟与日志。
//!
//! 桌面档那三个（密钥库、热键、托盘）在无屏档整块不存在，见 `platform/`；
//! 致命提示也不用弹框——systemd 会把退出状态记下来，journal 里有日志。

pub mod clock;
pub mod log;
