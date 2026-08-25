//! Bounded IEEE C37.118 TCP simulator primitives.
//!
//! The service intentionally has no WAMA gateway, Kafka, or Common Format
//! dependency. Wire interoperability against the approved IEEE 2024 evidence
//! remains a required release gate.

pub mod config;
pub mod identity;
pub mod management;
pub mod scenario;
pub mod server;
pub mod startup;
pub mod time_health;
pub mod wire_v2;
pub mod wire_v3;
