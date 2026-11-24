//! Pure model: identities, receipts, flows and spend. No IO lives here, so
//! identity derivation cannot depend on the machine it runs on.

pub mod attest;
pub mod compare;
pub mod digest;
pub mod fault;
pub mod flow;
pub mod metrics;
pub mod receipt;
pub mod secrets;
pub mod shell;
