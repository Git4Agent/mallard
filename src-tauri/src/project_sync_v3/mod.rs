//! Clean-break, project-scoped synchronization (schema 3).
//!
//! Local schema-3 state is isolated below the application-data `v3`
//! directory. Nothing in this module reads or rewrites schema-2 profile
//! configuration.

pub mod bundle_engine;
pub mod chat_history;
pub mod commands;
pub mod domain;
pub mod persistence;
pub mod provider_capture;
pub mod s3_store;
