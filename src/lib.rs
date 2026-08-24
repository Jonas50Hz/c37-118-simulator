//! Bounded IEEE C37.118 TCP simulator primitives.
//!
//! The service intentionally has no WAMA gateway, Kafka, or Common Format
//! dependency. Wire interoperability against the approved IEEE 2024 evidence
//! remains a required release gate.

pub mod config;
pub mod server;
pub mod wire_v2;
pub mod wire_v3;
