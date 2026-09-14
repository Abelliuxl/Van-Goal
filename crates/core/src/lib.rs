//! Backend-neutral core of Van-Goal: everything that does not need a UI.
//!
//! The desktop client (`crates/desktop`, GPUI) and the mobile client
//! (`crates/mobile` + the Flutter app) both build on this crate. Nothing here
//! may reach for a UI toolkit — that is the whole point of the split, and the
//! mobile build is what breaks first if it ever does.
//!
//! What lives here is the part that is expensive to get right and identical on
//! every platform: the seven backend protocol adapters in [`agent`], the
//! normalized event model in [`models`], the markdown parser in [`markdown`],
//! and the on-disk state in [`settings`] / [`cache`] / [`secret_store`].

pub mod agent;
pub mod cache;
pub mod hermes_config;
pub mod jsonl_process;
pub mod local_server;
pub mod logger;
pub mod markdown;
pub mod models;
pub mod secret_store;
pub mod settings;
