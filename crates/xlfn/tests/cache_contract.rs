#![cfg(feature = "cache")]

#[path = "cache_contract/basic.rs"]
mod basic;
#[path = "cache_contract/clear.rs"]
mod clear;
#[path = "cache_contract/errors.rs"]
mod errors;
#[path = "cache_contract/eviction.rs"]
mod eviction;
#[path = "cache_contract/generations.rs"]
mod generations;
#[path = "cache_contract/lease.rs"]
mod lease;
#[path = "cache_contract/registry.rs"]
mod registry;
#[path = "cache_contract/single_flight.rs"]
mod single_flight;
#[path = "cache_contract/weights.rs"]
mod weights;
