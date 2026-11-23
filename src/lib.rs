//! vor-ges/vrc-friend-bot のRust移植。Python版との対応は PORTING.md を参照。
pub mod bot;
pub mod db;
pub mod i18n;
pub mod import;
pub mod logic;
mod i18n_table;
pub mod stats;
pub mod vrc;
#[cfg(feature = "bot")]
pub mod discord;
#[cfg(feature = "bot")]
pub mod ws;
