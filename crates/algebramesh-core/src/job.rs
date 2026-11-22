use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Identifier assigned by AlgebraMesh. Unrelated to OpenXM serial numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
}

/// Automatic retries are allowed only for deterministic jobs without side effects,
/// and never assume exactly-once execution (design §7.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    #[default]
    Never,
    IfDeterministic,
}

/// Where a job may run (design §7.1). Empty lists mean "anywhere".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConstraints {
    /// Accepted CAS builds (image digests or source commits) as reported by agents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builds: Vec<String>,
    /// Accepted agent names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
}

impl EnvironmentConstraints {
    pub fn is_empty(&self) -> bool {
        self.builds.is_empty() && self.agents.is_empty()
    }

    pub fn allows(&self, agent_name: &str, build: &str) -> bool {
        (self.builds.is_empty() || self.builds.iter().any(|b| b == build))
            && (self.agents.is_empty() || self.agents.iter().any(|a| a == agent_name))
    }
}

/// A job as submitted by a client (design §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub schema_version: u32,
    pub backend: String,
    pub operation: String,
    pub arguments: serde_json::Value,
    #[serde(default)]
    pub limits: ResourceLimits,
    #[serde(default)]
    pub retry_policy: RetryPolicy,
    #[serde(default, skip_serializing_if = "EnvironmentConstraints::is_empty")]
    pub environment: EnvironmentConstraints,
}

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("cannot parse job specification: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unsupported schema_version {0}; this build understands 1")]
    SchemaVersion(u32),
    #[error("invalid job specification: {0}")]
    Invalid(String),
}

impl JobSpec {
    pub const SCHEMA_VERSION: u32 = 1;
    /// Smallest `limits.memory_mb`: no CAS starts in less.
    pub const MIN_MEMORY_MB: u64 = 16;
    /// Largest `limits.memory_mb` (1 PiB), so that the byte count fits in 64 bits.
    pub const MAX_MEMORY_MB: u64 = 1 << 30;
    /// Largest `limits.wall_seconds` (one year), so that a deadline can be
    /// represented on every platform (Issue #31).
    pub const MAX_WALL_SECONDS: u64 = 365 * 24 * 60 * 60;
    /// Largest length of `backend` and of `operation`, in bytes, so that a
    /// listing of jobs reads little of each (Issue #76).
    pub const MAX_NAME_BYTES: usize = 64;

    /// Reads a specification. An object with a key twice is refused at any
    /// depth: `arguments` becomes a `serde_json::Value`, which keeps only the
    /// last value of a key, so the typed parse of the arguments in a backend
    /// could not see the others (Issue #77).
    pub fn from_json(text: &str) -> Result<Self, SpecError> {
        let _: NoDuplicateKeys = serde_json::from_str(text)?;
        let spec: Self = serde_json::from_str(text)?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), SpecError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(SpecError::SchemaVersion(self.schema_version));
        }
        let is_name = |s: &str| {
            !s.is_empty()
                && s.split('.').all(|part| {
                    !part.is_empty()
                        && part
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                })
        };
        for (what, name) in [("backend", &self.backend), ("operation", &self.operation)] {
            if name.len() > Self::MAX_NAME_BYTES {
                return Err(SpecError::Invalid(format!(
                    "the {what} name has {} bytes, more than {}",
                    name.len(),
                    Self::MAX_NAME_BYTES
                )));
            }
        }
        if !is_name(&self.backend) {
            return Err(SpecError::Invalid(format!(
                "backend name {:?} must be lowercase ASCII",
                self.backend
            )));
        }
        if !is_name(&self.operation) {
            return Err(SpecError::Invalid(format!(
                "operation name {:?} must be dot-separated lowercase ASCII",
                self.operation
            )));
        }
        if let Some(s) = self.limits.wall_seconds
            && !(1..=Self::MAX_WALL_SECONDS).contains(&s)
        {
            return Err(SpecError::Invalid(format!(
                "limits.wall_seconds must be between 1 and {} (one year), not {s}",
                Self::MAX_WALL_SECONDS
            )));
        }
        if let Some(mb) = self.limits.memory_mb
            && !(Self::MIN_MEMORY_MB..=Self::MAX_MEMORY_MB).contains(&mb)
        {
            return Err(SpecError::Invalid(format!(
                "limits.memory_mb must be between {} and {} (MiB), not {mb}",
                Self::MIN_MEMORY_MB,
                Self::MAX_MEMORY_MB
            )));
        }
        Ok(())
    }
}

/// Any JSON value in which no object has a key twice, at any depth.
struct NoDuplicateKeys;

impl<'de> Deserialize<'de> for NoDuplicateKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NoDuplicateKeysVisitor)
    }
}

struct NoDuplicateKeysVisitor;

impl<'de> serde::de::Visitor<'de> for NoDuplicateKeysVisitor {
    type Value = NoDuplicateKeys;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(NoDuplicateKeys)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<NoDuplicateKeys>()?.is_some() {}
        Ok(NoDuplicateKeys)
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut keys = std::collections::HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if keys.contains(&key) {
                let key = if key.len() <= JobSpec::MAX_NAME_BYTES {
                    format!("{key:?}")
                } else {
                    format!("of {} bytes", key.len())
                };
                return Err(serde::de::Error::custom(format!(
                    "the key {key} appears twice in one object"
                )));
            }
            map.next_value::<NoDuplicateKeys>()?;
            keys.insert(key);
        }
        Ok(NoDuplicateKeys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESIGN_EXAMPLE: &str = r#"{
      "schema_version": 1,
      "backend": "asir",
      "operation": "polynomial.factor",
      "arguments": {
        "ring": {"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
        "expression": {"type": "polynomial", "terms": [
          {"coefficient": "1", "exponents": [4, 0]},
          {"coefficient": "-1", "exponents": [0, 4]}
        ]}
      },
      "limits": {"wall_seconds": 120, "memory_mb": 1024},
      "retry_policy": "never"
    }"#;

    #[test]
    fn wall_seconds_must_be_representable() {
        let with = |s: u64| {
            DESIGN_EXAMPLE.replace(r#""wall_seconds": 120"#, &format!(r#""wall_seconds": {s}"#))
        };
        for bad in [0, JobSpec::MAX_WALL_SECONDS + 1, u64::MAX] {
            assert!(JobSpec::from_json(&with(bad)).is_err(), "{bad}");
        }
        assert!(JobSpec::from_json(&with(1)).is_ok());
        assert!(JobSpec::from_json(&with(JobSpec::MAX_WALL_SECONDS)).is_ok());
        // A context with an unrepresentable timeout does not panic.
        let ctx = crate::ExecutionContext::new(Some(std::time::Duration::from_secs(u64::MAX)));
        assert!(ctx.deadline.is_none());
    }

    #[test]
    fn memory_limits_must_be_in_range() {
        let with = |mb: u64| {
            DESIGN_EXAMPLE.replace(r#""memory_mb": 1024"#, &format!(r#""memory_mb": {mb}"#))
        };
        assert!(JobSpec::from_json(&with(0)).is_err());
        assert!(JobSpec::from_json(&with(8)).is_err());
        assert!(JobSpec::from_json(&with(1 << 31)).is_err());
        assert!(JobSpec::from_json(&with(16)).is_ok());
        assert!(JobSpec::from_json(&with(1024)).is_ok());
    }

    #[test]
    fn parses_design_example() {
        let spec = JobSpec::from_json(DESIGN_EXAMPLE).unwrap();
        assert_eq!(spec.backend, "asir");
        assert_eq!(spec.operation, "polynomial.factor");
        assert_eq!(spec.limits.wall_seconds, Some(120));
        assert_eq!(spec.retry_policy, RetryPolicy::Never);
    }

    #[test]
    fn rejects_unknown_fields_and_versions() {
        let extra =
            DESIGN_EXAMPLE.replace("\"retry_policy\"", "\"script\": \"x\", \"retry_policy\"");
        assert!(matches!(
            JobSpec::from_json(&extra),
            Err(SpecError::Parse(_))
        ));
        let v2 = DESIGN_EXAMPLE.replace("\"schema_version\": 1", "\"schema_version\": 2");
        assert!(matches!(
            JobSpec::from_json(&v2),
            Err(SpecError::SchemaVersion(2))
        ));
        let bad_op = DESIGN_EXAMPLE.replace("polynomial.factor", "fctr(x); shell(\\\"rm\\\")");
        assert!(matches!(
            JobSpec::from_json(&bad_op),
            Err(SpecError::Invalid(_))
        ));
    }

    /// Issue #76: a name of 2 MiB passes the check of its characters and the
    /// bound of the specification, but not the bound of a name.
    #[test]
    fn names_are_short() {
        let longest = "a".repeat(JobSpec::MAX_NAME_BYTES);
        let backend = DESIGN_EXAMPLE.replace("\"asir\"", &format!("\"{longest}\""));
        assert_eq!(JobSpec::from_json(&backend).unwrap().backend, longest);
        let operation = DESIGN_EXAMPLE.replace("polynomial.factor", &format!("p.{longest}"));
        let Err(SpecError::Invalid(message)) = JobSpec::from_json(&operation) else {
            panic!("an operation name of 66 bytes was accepted");
        };
        assert!(message.contains("more than 64"), "{message}");
        let huge = DESIGN_EXAMPLE.replace("\"asir\"", &format!("\"{}\"", "a".repeat(2 << 20)));
        let Err(SpecError::Invalid(message)) = JobSpec::from_json(&huge) else {
            panic!("a backend name of 2 MiB was accepted");
        };
        assert!(message.len() < 100, "the message repeats the name");
    }

    /// Issue #77: the specification of the review, with `weights` twice. As a
    /// `serde_json::Value` it keeps only `[1, 0]`; the negative weight would
    /// never reach the typed parse of the ring.
    #[test]
    fn a_key_twice_is_refused_at_any_depth() {
        let twice = r#"{
          "schema_version": 1,
          "backend": "macaulay2",
          "operation": "polynomial.factor",
          "arguments": {
            "ring": {
              "domain": "QQ",
              "variables": ["x", "y"],
              "order": {
                "weights": [-1, 0],
                "weights": [1, 0],
                "then": "lex"
              }
            },
            "expression": {
              "type": "polynomial",
              "terms": [
                {"coefficient": "1", "exponents": [2, 0]},
                {"coefficient": "-1", "exponents": [0, 1]}
              ]
            }
          }
        }"#;
        let value: serde_json::Value = serde_json::from_str(twice).unwrap();
        assert_eq!(
            value["arguments"]["ring"]["order"],
            serde_json::json!({"weights": [1, 0], "then": "lex"}),
            "a Value drops the first weights"
        );
        let refused = |text: &str| match JobSpec::from_json(text) {
            Err(SpecError::Parse(e)) => assert!(e.to_string().contains("twice"), "{e}"),
            other => panic!("accepted {text}: {other:?}"),
        };
        refused(twice);

        let template = r#"{"schema_version": 1, "backend": "macaulay2",
            "operation": "polynomial.factor", "arguments": {
            "ring": {"domain": "QQ", "variables": ["x", "y"], "order": ORDER},
            "expression": {"type": "polynomial",
                           "terms": [{"coefficient": "1", "exponents": [2, 0]}]}}}"#;
        let order = |o: &str| template.replace("ORDER", o);
        let accepted = [
            r#""grevlex""#,
            r#"{"weights": [1, 0], "then": "lex"}"#,
            r#"{"block": [{"count": 1, "order": "grevlex"}, {"count": 1, "order": "lex"}]}"#,
        ];
        for o in accepted {
            let spec = JobSpec::from_json(&order(o)).unwrap();
            let read: serde_json::Value = serde_json::from_str(o).unwrap();
            assert_eq!(spec.arguments["ring"]["order"], read);
        }
        for o in [
            r#"{"weights": [1, 0], "then": "lex", "then": "grevlex"}"#,
            r#"{"block": [], "block": [{"count": 2, "order": "lex"}]}"#,
            r#"{"block": [{"count": 1, "count": 1, "order": "lex"}, {"count": 1, "order": "lex"}]}"#,
            r#"{"block": [{"count": 2, "order": "lex", "order": "grevlex"}]}"#,
        ] {
            refused(&order(o));
        }
        // At the top and in the limits as well; the same key in two objects
        // is not twice.
        refused(&DESIGN_EXAMPLE.replace(
            "\"retry_policy\"",
            "\"backend\": \"asir\", \"retry_policy\"",
        ));
        refused(&DESIGN_EXAMPLE.replace(
            "\"memory_mb\": 1024",
            "\"memory_mb\": 1024, \"memory_mb\": 1024",
        ));
        assert!(
            JobSpec::from_json(DESIGN_EXAMPLE).is_ok(),
            "two objects have \"coefficient\""
        );
    }
}
