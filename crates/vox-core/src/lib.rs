//! Vox 内核：平台无关。
//!
//! 不 `use windows::*`，不碰 Tauri，不知道 WASAPI 存在。要用平台能力时只认
//! [`ports`] 里的 trait，由 Windows 外壳在启动时注入实现。
//!
//! 模块地图见 `docs/architecture/ARCHITECTURE.md`。

pub mod capability;
pub mod catalog;
pub mod cloud;
pub mod composition;
pub mod event;
pub mod gate;
pub mod hotkey;
pub mod latency;
pub mod pipeline;
pub mod ports;
pub mod runtime;
pub mod settings;
pub mod subtitle;
pub mod usage;

pub use capability::{
    Capability, CapabilityReport, CapabilityScope, CapabilitySet, CapabilityStatus, HostFacts,
    UnavailableReason,
};
pub use catalog::ActivationMode;
pub use cloud::{
    Backoff, HotChange, Incoming, ParsedEvent, ServerEvent, Session, SessionParams, Transport,
};
pub use composition::{
    Composition, CompositionError, Control, EdgeSource, HostKind, Input, Life, Op, Output,
    PlaybackRole, RateRef, SessionSpec, Ui, COMPOSITION_SCHEMA_VERSION,
};
pub use event::{Event, Notice, Pipeline, PipelineState, Severity};
pub use gate::{ActivationGate, GateConfig, GateKind, GateState, GateStatus};
pub use hotkey::Hotkey;
pub use latency::{LatencySnapshot, MetricSummary};
pub use pipeline::{
    CaptureFactory, DenoiseFactory, Deps, PipelineEngine, PlaybackFactory, ResampleFactory,
    TransportFactory,
};
pub use ports::{Denoise, HotkeyEvent, PortError, PortResult, Resample};
pub use runtime::{PipelineCommand, PipelineControl, Runtime, SessionConfig, Snapshot};
pub use settings::Settings;
pub use subtitle::{Subtitles, Track};
pub use usage::{TurnUsage, UsageLedger};
