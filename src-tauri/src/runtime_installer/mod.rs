//! Native runtime/model installation primitives (issue #107).
//!
//! These modules deliberately know nothing about Tauri or SA3. The model
//! manager supplies the authenticated, application-bundled manifest and owns
//! progress/cancellation; these helpers enforce the filesystem/network trust
//! boundary and make the final swap recoverable.

pub(crate) mod archive;
pub(crate) mod download;
pub(crate) mod promotion;
