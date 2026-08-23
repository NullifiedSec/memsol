pub mod attention;
pub mod hyprland;
pub mod learning;
pub mod model;
pub mod policy;
pub mod telemetry;

pub use attention::{AttentionGraph, AttentionState, WindowState, WorkspaceState};
pub use hyprland::{
    HyprlandEvent, HyprlandEventStream, HyprlandPaths, apply_event, parse_event, snapshot_attention,
};
pub use learning::{AttentionLearner, AttentionObservation, ObservationKind, Prediction};
pub use model::{MemorySnapshot, PressureLevel, PsiLine, PsiMemory};
pub use policy::classify_pressure;
