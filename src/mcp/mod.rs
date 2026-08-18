//! MCP-сервер и протокол интеграции с Hermes.

pub mod protocol;
pub mod server;
pub mod tools;

pub use server::{AppContext, McpServer};
