//! flot — a verifiable agent flow runtime.
//!
//! flot runs a declared DAG of commands (shell steps, agent CLIs, test suites)
//! and records a content-addressed receipt for each one. A receipt states what
//! ran, over which input content, after which upstream receipts, what it
//! produced, which claims it makes, and what command proved them. Reuse,
//! comparison and audit all derive from those digests.
//!
//! The contract flot enforces mechanically: **a claim is unverified until an
//! independent command proves it**, so a run with unproven claims can never
//! report `passed`.
//!
//! Layers, enforced by `tests/architecture.rs`; dependencies only point down:
//! `core` <- `store` <- `engine` <- `service` <- `adapters`.

pub mod adapters;
pub mod core;
pub mod engine;
pub mod service;
pub mod store;
