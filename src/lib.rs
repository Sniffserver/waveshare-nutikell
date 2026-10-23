// Host-side library target — only used for `cargo test`.
// The embedded binary entry-point is in src/main.rs.
#![cfg_attr(not(test), no_std)]

pub mod outdoor {
    pub mod steps;
    pub mod packet;
}
