pub mod classifier;
pub mod matrix;
pub mod model;
pub mod registry;

pub use classifier::{ClassifiedSurface, SurfaceClassifier};
pub use matrix::CompatibilityMatrix;
pub use model::{
    ApprovalSensitivity, AsyncMode, AvailabilityGate, FallbackMode, HostDependency, InvocationMode,
    MappingDecision, MappingStrategy, OperationMode, SideEffectLevel, StateScope, SurfaceBucket,
    SurfaceDescriptor, SurfaceFamily, SurfaceKind, UnsupportedReason,
};
pub use registry::SurfaceRegistry;
