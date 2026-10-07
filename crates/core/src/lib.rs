//! UI-independent core of yt-lite. Everything here is blocking and
//! thread-safe; front-ends (GPUI desktop, a future SwiftUI app via FFI) run it
//! off their UI thread.

pub mod auth;
pub mod config;
pub mod db;
pub mod feed;
pub mod lru;
pub mod net;
pub mod shorts;
pub mod thumbcache;
pub mod youtube;
