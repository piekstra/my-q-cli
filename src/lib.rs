//! The library behind `myq`: sign-in, the myQ cloud client, and the MCP tools.
//!
//! Everything that talks to the provider is written against
//! `pk_cli_core::transport::Transport`, so the tests run the whole flow offline
//! against scripted responses.

pub mod auth;
pub mod client;
pub mod config;
pub mod credential;
pub mod door;
pub mod hub;
pub mod mcp;
pub mod phone;
pub mod signin;

pub const BIN: &str = "myq";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
