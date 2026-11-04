// The bundle's Cargo library, for `cargo test` without the game: each module the bundle adds to
// crates/horizon-client/src/game.rs, under the name horizon-mixin.toml gives it. Not part of the
// bundle itself.

#[path = "runtime.rs"]
pub mod port_ae2;

#[path = "core.rs"]
pub mod port_ae2_core;

#[path = "crafting.rs"]
pub mod port_ae2_crafting;

#[path = "execution.rs"]
pub mod port_ae2_execution;

#[path = "grid.rs"]
pub mod port_ae2_grid;
