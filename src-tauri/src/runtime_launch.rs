//! Runtime-launch policy shared by every Python-backed service.
//!
//! Developer builds may use the source-tree `uv run ...` commands. Portable
//! release builds must not: their MRT2 and Stable Audio adapters are installed
//! and verified in app-owned storage, then expose an explicit executable path.
//! Keeping this decision in one tiny module makes the #110/#111 adapters
//! pluggable without duplicating a dangerous fallback at each call site.

use std::io;

/// Whether commands may fall back to source-tree developer tooling when no
/// explicit backend executable has been configured.
pub const fn developer_fallback_allowed() -> bool {
    !cfg!(feature = "managed-runtime")
}

/// A structured launch failure for a packaged build whose platform adapter is
/// not installed yet. Callers already surface command-spawn failures through
/// their bounded diagnostics/status contracts.
pub fn unavailable(service: &str) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("runtime.unavailable.{service}"))
}

/// Stable diagnostic identifier for the launch policy in this build.
pub const fn mode() -> &'static str {
    if cfg!(feature = "bundled-backend") {
        "bundled"
    } else if cfg!(feature = "managed-runtime") {
        "managed"
    } else {
        "developer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_mode_matches_the_compiled_contract() {
        if cfg!(feature = "managed-runtime") {
            assert_eq!(mode(), "managed");
            assert!(!developer_fallback_allowed());
        } else if cfg!(feature = "bundled-backend") {
            assert_eq!(mode(), "bundled");
            assert!(developer_fallback_allowed());
        } else {
            assert_eq!(mode(), "developer");
            assert!(developer_fallback_allowed());
        }
    }

    #[test]
    fn missing_managed_runtime_is_an_actionable_not_found_error() {
        let error = unavailable("mrt2");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(error.to_string(), "runtime.unavailable.mrt2");
    }
}
