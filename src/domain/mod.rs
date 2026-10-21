//! The pure core: no I/O, no clock, no randomness. Everything here is a
//! function of its arguments (DESIGN.md §2).

pub mod dedupe;
pub mod model;
pub mod proposals;
pub mod quizfill;
pub mod rules;
pub mod text;
pub mod typo_guard;
pub mod voting;
