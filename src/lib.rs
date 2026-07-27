//! The viewer: a read-only GitHub issue pane for herdr.
//!
//! The one seam every test drives is [`app::App`] — constructed from an
//! [`environment::Environment`] description, fed key events, rendered into a
//! terminal backend. Everything below that seam is real: a real HTTP client, a
//! real `git`, a real repo root. Only the GitHub endpoint is substitutable, via
//! [`environment::Environment::graphql_url`].

pub mod age;
pub mod app;
pub mod environment;
pub mod github;
pub mod identity;
pub mod signals;
pub mod ui;
