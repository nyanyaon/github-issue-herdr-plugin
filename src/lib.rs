//! The viewer: a read-only GitHub issue pane for herdr.
//!
//! The one seam every test drives is [`app::App`] — constructed from an
//! [`environment::Environment`] description, fed key events, rendered into a
//! terminal backend. Everything below that seam is real: a real HTTP client, a
//! real `git`, a real repo root. Only two endpoints are substitutable — the
//! GitHub API via [`environment::Environment::graphql_url`] and the herdr socket
//! via [`environment::Environment::herdr_socket`] — and the tests substitute
//! both with real local servers rather than mocks.

pub mod age;
pub mod app;
pub mod cache;
pub mod config;
pub mod environment;
pub mod github;
pub mod herdr;
pub mod identity;
pub mod signals;
pub mod ui;
