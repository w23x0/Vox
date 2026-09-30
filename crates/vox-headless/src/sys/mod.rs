//! 跟操作系统直接打交道的小件：日志。
//!
//! 时钟（`LocalClock`）与三个密钥后端都在 `vox_host`（S4-A，与桌面 Linux 侧共用同一份）；
//! 桌面档另外那几个（热键、托盘）在无屏档整块不存在，见 `platform/`；
//! 致命提示也不用弹框——systemd 会把退出状态记下来，journal 里有日志。

pub mod log;
