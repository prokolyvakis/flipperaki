//! The single native Flipper protobuf RPC connection: framing and the session
//! executor. Generated protobuf types come from `flipper-companion-protocol`.

mod codec;
mod session;

pub use codec::{FrameError, RpcCodec, encode_frame};
pub use flipper_companion_protocol::{
    ScreenFrame, pb, pb_app, pb_desktop, pb_gui, pb_property, pb_storage, pb_system,
};
pub use session::{
    AppState, Delivery, DeviceEvent, Failure, Reason, Reply, RequestKind, RpcSession,
    SessionLimits, SessionState, Stage,
};
