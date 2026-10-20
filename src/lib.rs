#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented,
        clippy::unreachable
    )
)]

pub mod adapter;
pub mod bundle;
pub mod inventory;
pub mod io;
pub mod migrate;
pub mod model;
pub mod source;
mod sqlite;
