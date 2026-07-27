//! The viewer: a read-only GitHub issue pane for herdr.
//!
//! The one seam every test drives is the app — constructed from an environment
//! description, fed key events, rendered into a terminal backend. Everything
//! below that seam is real: a real HTTP client, a real `git`, a real repo root.
//! Only the GitHub endpoint is substitutable.
