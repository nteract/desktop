// Tests are allowed to use unwrap()/expect()—they're how you assert
// preconditions and keep test failures informative. Workspace-wide
// `clippy::unwrap_used = "warn"` applies to non-test code.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod connection;
pub mod protocol;
