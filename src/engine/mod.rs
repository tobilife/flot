//! Turning a flow into receipts: input keying, process execution, claim
//! settlement and dependency scheduling.

pub mod claims;
pub mod exec;
pub mod keying;
pub mod runner;
pub mod scheduler;
