//! `vpsagent-core`: общие типы, протокол JSON-RPC, конфигурация.

pub mod config;
pub mod error;
pub mod protocol;
pub mod types;

pub use config::{Config, EndpointKind, ModelEndpoint, Paths, PermissionMode};
pub use error::{Error, Result};
pub use protocol::{
    Event, EventKind, JsonRpc, Request, RequestId, Response, RpcError, PROTOCOL_VERSION,
};
pub use types::{
    AgentInfo, AgentKind, AgentStatus, ContentBlock, DaemonStatus, Id, Message, Role, Session,
    SessionStatus, TokenUsage,
};
