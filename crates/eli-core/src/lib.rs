#![forbid(unsafe_code)]

pub mod adapter;
pub mod agent;
pub mod config;
pub mod contract;
pub mod diff;
pub mod executor;
pub mod extraction;
pub mod finance;
pub mod legal;
pub mod memory;
pub mod meta;
pub mod metrics;
pub mod openrouter;
pub mod orchestrator;
pub mod persistence;
pub mod trajectory;
pub mod types;
pub mod web;

mod error;

pub use adapter::{AdapterError, ChatStream, LlmAdapter};
pub use error::{Error, Result};
