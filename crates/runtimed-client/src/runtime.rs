//! Runtime type for notebooks - Python or Deno
//!
//! Unknown values (e.g. from a newer branch) are preserved via `Other(String)`
//! so they round-trip through settings without being silently replaced.

use schemars::JsonSchema;
use ts_rs::TS;

/// Supported notebook runtime environments.
///
/// Unknown values are captured in the `Other` variant so they survive
/// serialization round-trips across branches that add new runtimes.
#[derive(Debug, Clone, PartialEq, Eq, Default, TS)]
#[ts(export)]
#[ts(type = "\"python\" | \"deno\" | (string & {})")]
pub enum Runtime {
    #[default]
    Python,
    Deno,
    /// An unrecognized runtime value, preserved for round-tripping.
    Other(String),
}

// ── Serde ────────────────────────────────────────────────────────────

impl serde::Serialize for Runtime {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for Runtime {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(s.parse().expect("FromStr for Runtime is infallible"))
    }
}

// ── JSON Schema ─────────────────────────────────────────────────────

impl JsonSchema for Runtime {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Runtime".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "examples": ["python", "deno"]
        })
    }
}

// ── Display / FromStr ───────────────────────────────────────────────

impl std::fmt::Display for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Runtime::Python => write!(f, "python"),
            Runtime::Deno => write!(f, "deno"),
            Runtime::Other(s) => write!(f, "{}", s),
        }
    }
}

impl std::str::FromStr for Runtime {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "python" | "py" => Runtime::Python,
            "deno" | "typescript" | "ts" => Runtime::Deno,
            _ => Runtime::Other(s.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_display() {
        assert_eq!(Runtime::Python.to_string(), "python");
        assert_eq!(Runtime::Deno.to_string(), "deno");
        assert_eq!(Runtime::Other("julia".into()).to_string(), "julia");
    }

    #[test]
    fn test_runtime_from_str() {
        assert_eq!("python".parse::<Runtime>().unwrap(), Runtime::Python);
        assert_eq!("py".parse::<Runtime>().unwrap(), Runtime::Python);
        assert_eq!("deno".parse::<Runtime>().unwrap(), Runtime::Deno);
        assert_eq!("typescript".parse::<Runtime>().unwrap(), Runtime::Deno);
        assert_eq!("ts".parse::<Runtime>().unwrap(), Runtime::Deno);
        // Unknown values are preserved, not errors
        assert_eq!(
            "julia".parse::<Runtime>().unwrap(),
            Runtime::Other("julia".into())
        );
    }

    #[test]
    fn test_runtime_default() {
        assert_eq!(Runtime::default(), Runtime::Python);
    }

    #[test]
    fn test_runtime_serde() {
        assert_eq!(
            serde_json::to_string(&Runtime::Python).unwrap(),
            "\"python\""
        );
        assert_eq!(serde_json::to_string(&Runtime::Deno).unwrap(), "\"deno\"");
        assert_eq!(
            serde_json::from_str::<Runtime>("\"python\"").unwrap(),
            Runtime::Python
        );
        assert_eq!(
            serde_json::from_str::<Runtime>("\"deno\"").unwrap(),
            Runtime::Deno
        );
    }

    #[test]
    fn test_runtime_serde_round_trip_unknown() {
        let julia = Runtime::Other("julia".into());
        let json = serde_json::to_string(&julia).unwrap();
        assert_eq!(json, "\"julia\"");
        let back: Runtime = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Runtime::Other("julia".into()));
    }
}
