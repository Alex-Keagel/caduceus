---
name: rust-serde-serialization
version: "1.0"
description: JSON, TOML, and YAML serialization/deserialization with serde in Rust
categories: [rust, serialization, data]
triggers: ["serde json", "serialize rust", "deserialize rust struct", "serde derive macro", "toml yaml serde"]
tools: [read_file, edit_file, run_tests, shell]
---

# Rust Serde Serialization Skill

## Dependencies
```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
serde_yaml = "0.9"
```

## Basic Derive
```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    server_port: u16,
    #[serde(default)]
    debug_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
}
```

## Key Attributes
| Attribute | Effect |
|-----------|--------|
| `rename_all = "camelCase"` | Rename all fields to camelCase |
| `rename = "name"` | Rename one specific field |
| `skip_serializing_if = "Option::is_none"` | Omit None fields from output |
| `default` | Use `Default::default()` when field is absent |
| `flatten` | Merge nested struct into parent JSON object |
| `tag`, `content` | Adjacently tagged enum representation |

## Enum Serialization
```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Created { id: u64 },
    Deleted { id: u64, reason: String },
}
```

## JSON Operations
```rust
// Serialize to pretty-printed JSON string
let json = serde_json::to_string_pretty(&value)?;

// Deserialize from JSON string
let config: Config = serde_json::from_str(&json_str)?;

// Work with dynamic JSON
let v: serde_json::Value = serde_json::from_str(raw)?;
let port = v["server"]["port"].as_u64().unwrap_or(8080);
```

## TOML Config Files
```rust
let content = std::fs::read_to_string("config.toml")?;
let config: Config = toml::from_str(&content)?;
```

## Custom Serializers
- Use `#[serde(with = "module")]` to delegate to a helper module for partial overrides
- Implement `Serialize`/`Deserialize` traits manually using the visitor pattern for external types
- Use `#[serde(remote = "ExternalType")]` for types you don't own

## Testing
- Round-trip test: serialize then deserialize and assert equality with original
- Test with missing/extra/null fields to verify `default` and `deny_unknown_fields` behavior
- Snapshot-test JSON output with `insta` crate to catch accidental schema changes
