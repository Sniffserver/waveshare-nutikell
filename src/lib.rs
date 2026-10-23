// Host-side library target — used for host-side unit testing (`cargo test-host`).
// The embedded binary entry-point is in src/main.rs.
#![cfg_attr(not(test), no_std)]

pub mod outdoor {
    pub mod steps;
    pub mod packet;
    pub mod solar;
    pub mod state;
}
