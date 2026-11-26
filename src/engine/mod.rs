//! Turning a flow into receipts: input keying, process execution, claim
//! settlement and dependency scheduling, and the progress a run reports.

pub mod baseline;
pub mod claims;
pub mod exec;
pub mod git;
pub mod keying;
pub mod progress;
pub mod runner;
pub mod sandbox;
pub mod scheduler;
