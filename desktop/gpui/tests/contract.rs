//! The contract between the typed client and the gateway.
//!
//! The shell decodes the gateway's JSON with `serde`; a renamed or dropped
//! field is silently decoded as a default (a combo that claims zero
//! servable entries, a usage chart that claims no tokens). This test reads
//! both sides of the contract out of the source and checks that they still
//! match:
//!
//! * the client — `desktop/gpui/src/api.rs`, the `serde` field names of every
//!   response struct;
//! * the gateway — `crates/heterion-router-gateway/src/admin.rs` (and
//!   `lib.rs` for `/healthz`), the keys of the `json!` objects every handler
//!   answers with.
//!
//! The check is deliberately scoped to the **top-level keys** of each
//! response (plus the keys of the inline nested objects and of the row
//! objects). The gateway builds its rows in a second `json!` literal, so the
//! row keys are checked against the union of the handler's row literals.
//! A full structural diff would need a Rust parser; this one needs a
//! `read_line`.

// The helpers below are only used by some of the checks.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The directory of the `heterion-router-desktop` crate.
const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Fields the client knows but the gateway does not produce.
///
/// Empty on purpose: every field of a response struct must exist on the
/// gateway side. A field that is deliberately client-only belongs here, so
/// the exception stays visible.
const CLIENT_ONLY: &[(&str, &str)] = &[];

/// Fields the gateway sets only in the failure variants of a response.
///
/// They are checked against every variant the handler answers with, not
/// against the variants that carry a payload.
const ERROR_ONLY: &[&str] = &["error", "connection_error"];

/// What the client expects from one endpoint.
struct Expectation {
    /// The gateway file the handler lives in.
    file: &'static str,
    /// The handler in that file.
    handler: &'static str,
    /// The client struct of the top-level response.
    response: &'static str,
    /// The client structs of the rows of the response.
    rows: &'static [(&'static str, &'static str)],
    /// The client structs of the inline nested objects, by their key.
    nested: &'static [(&'static str, &'static str)],
}

/// Every endpoint the shell reads.
const EXPECTATIONS: &[Expectation] = &[
    Expectation {
        file: "lib.rs",
        handler: "healthz",
        response: "Health",
        rows: &[],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "overview",
        response: "Overview",
        rows: &[],
        nested: &[("counts", "OverviewCounts")],
    },
    Expectation {
        file: "admin.rs",
        handler: "combos",
        response: "CombosResponse",
        rows: &[("combo", "Combo"), ("combo entry", "ComboEntry")],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "connections",
        response: "ConnectionsResponse",
        rows: &[("connection", "Connection")],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "providers",
        response: "ProviderCatalog",
        rows: &[("provider", "ProviderInfo")],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "create_connection",
        response: "CreateConnectionResponse",
        rows: &[],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "keys",
        response: "KeysResponse",
        rows: &[("key", "ApiKey")],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "create_key",
        response: "CreatedKey",
        rows: &[],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "set_key_revoked",
        response: "KeyStateResponse",
        rows: &[],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "restart_service",
        response: "RestartResponse",
        rows: &[],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "usage",
        response: "Usage",
        rows: &[("usage bucket", "UsageBucket")],
        nested: &[("latency", "Latency")],
    },
    Expectation {
        file: "admin.rs",
        handler: "logs",
        response: "LogsResponse",
        rows: &[("call log", "CallLog")],
        nested: &[],
    },
    Expectation {
        file: "admin.rs",
        handler: "logs_app",
        response: "AppLogsResponse",
        rows: &[],
        nested: &[],
    },
];

/// One field of a client struct.
struct Field {
    name: String,
    /// `false` when the field carries `#[serde(default)]`.
    required: bool,
}

/// One response struct of the client.
struct ClientStruct {
    name: String,
    fields: Vec<Field>,
    /// The `#[derive(…)]` line above the struct.
    derives: String,
}

/// The keys the gateway produces for one handler.
struct Gateway {
    /// The keys of every response variant.
    top: BTreeSet<String>,
    /// The keys of the variants that carry a payload.
    payload: Vec<BTreeSet<String>>,
    /// The keys of the row objects.
    rows: BTreeSet<String>,
    /// The keys of the inline nested objects, by their path.
    nested: BTreeMap<String, BTreeSet<String>>,
}

#[test]
fn every_field_the_client_decodes_is_produced_by_the_gateway() {
    let client = parse_client_structs(&client_source());
    let mut sources: BTreeMap<&str, String> = BTreeMap::new();

    for expectation in EXPECTATIONS {
        let source = sources
            .entry(expectation.file)
            .or_insert_with(|| gateway_source(expectation.file));
        let handler = gateway_handler(source, expectation.handler).unwrap_or_else(|| {
            panic!(
                "the gateway must still have a handler `{}` in {}",
                expectation.handler, expectation.file
            )
        });

        let response = find_struct(&client, expectation.response).unwrap_or_else(|| {
            panic!(
                "the client must still have a struct `{}`",
                expectation.response
            )
        });
        assert_response_fields(
            &format!("/api/{} response", expectation.handler),
            response,
            &handler,
        );
        for (label, name) in expectation.rows {
            let row = find_struct(&client, name)
                .unwrap_or_else(|| panic!("the client must still have a struct `{name}`"));
            assert_fields(
                &format!("/api/{} {label}", expectation.handler),
                row,
                &handler.rows,
            );
        }

        for (key, name) in expectation.nested {
            let nested = find_struct(&client, name)
                .unwrap_or_else(|| panic!("the client must still have a struct `{name}`"));
            let available = handler.nested.get(*key).unwrap_or_else(|| {
                panic!(
                    "the gateway must still answer /api/{} with an inline `{key}` object",
                    expectation.handler
                )
            });
            assert_fields(
                &format!("/api/{} {key}", expectation.handler),
                nested,
                available,
            );
        }
    }
}

#[test]
fn every_response_struct_of_the_client_is_guarded() {
    // A new response struct in the client that no expectation knows about
    // would silently escape the contract check.
    let client = parse_client_structs(&client_source());

    let guarded: BTreeSet<&str> = EXPECTATIONS
        .iter()
        .flat_map(|expectation| {
            std::iter::once(expectation.response)
                .chain(expectation.rows.iter().map(|(_, name)| *name))
                .chain(expectation.nested.iter().map(|(_, name)| *name))
        })
        .collect();

    let decoded: BTreeSet<&str> = client
        .iter()
        .filter(|structure| structure.derives.contains("Deserialize"))
        .map(|structure| structure.name.as_str())
        .collect();

    let unguarded: Vec<&str> = decoded.difference(&guarded).copied().collect();
    assert!(
        unguarded.is_empty(),
        "these response structs are decoded by the client but not guarded by the contract: {unguarded:?}"
    );

    let missing: Vec<&str> = guarded.difference(&decoded).copied().collect();
    assert!(
        missing.is_empty(),
        "the contract guards structs the client does not decode: {missing:?}"
    );
}

/// Check that every field of the top-level response struct exists on the
/// gateway side.
///
/// Every variant that carries a payload must have the field: a handler that
/// answers with an empty list in one branch and with the real list in
/// another must not rename a field in only one of them — the client would
/// decode the empty branch and show nothing.
fn assert_response_fields(label: &str, structure: &ClientStruct, gateway: &Gateway) {
    assert!(
        !gateway.payload.is_empty(),
        "{label}: the gateway must answer with at least one variant that carries a payload"
    );

    for field in &structure.fields {
        if CLIENT_ONLY.contains(&(structure.name.as_str(), field.name.as_str())) {
            continue;
        }

        let missing = if ERROR_ONLY.contains(&field.name.as_str()) {
            // A failure field: it only has to exist somewhere.
            (!gateway.top.contains(&field.name)).then_some(0)
        } else {
            gateway
                .payload
                .iter()
                .position(|variant| !variant.contains(&field.name))
        };

        assert!(
            missing.is_none(),
            "{label}: the gateway does not produce `{}` (client struct `{}`, {})",
            field.name,
            structure.name,
            if field.required {
                "required"
            } else {
                "optional"
            }
        );
    }
}

/// Check that every field of a client struct exists on the gateway side.
fn assert_fields(label: &str, structure: &ClientStruct, available: &BTreeSet<String>) {
    for field in &structure.fields {
        if CLIENT_ONLY.contains(&(structure.name.as_str(), field.name.as_str())) {
            continue;
        }
        assert!(
            available.contains(&field.name),
            "{label}: the gateway does not produce `{}` (client struct `{}`, {})",
            field.name,
            structure.name,
            if field.required {
                "required"
            } else {
                "optional"
            }
        );
    }
}

/// Find a client struct by name.
fn find_struct<'a>(structs: &'a [ClientStruct], name: &str) -> Option<&'a ClientStruct> {
    structs.iter().find(|structure| structure.name == name)
}

/// The source of the typed client.
fn client_source() -> String {
    let path = Path::new(MANIFEST_DIR).join("src/api.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
    strip_comments(&source)
}

/// The source of a file of the gateway.
fn gateway_source(file: &str) -> String {
    let path = repository_root()
        .join("crates/heterion-router-gateway/src")
        .join(file);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
    strip_comments(&source)
}

/// Remove the comments of a Rust source, keeping every string literal.
///
/// The gateway writes comments inside its `json!` literals — a comment may
/// hold a comma or a brace — so they have to go before the literals are
/// parsed. The strings are kept verbatim: a `//` inside one is not a comment.
fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'r' if is_raw_string(bytes, index) => {
                let Some((_, next)) = read_raw_string(bytes, index) else {
                    break;
                };
                out.push_str(&source[index..next]);
                index = next;
            }
            b'"' => {
                let Some((_, next)) = read_string(bytes, index) else {
                    break;
                };
                out.push_str(&source[index..next]);
                index = next;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len() {
                    if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        index += 2;
                        break;
                    }
                    if bytes[index] == b'\n' {
                        out.push('\n');
                    }
                    index += 1;
                }
            }
            _ => {
                let mut end = index + 1;
                while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
                    end += 1;
                }
                out.push_str(&source[index..end]);
                index = end;
            }
        }
    }

    out
}

/// `true` when a raw string literal starts at `index`.
fn is_raw_string(bytes: &[u8], index: usize) -> bool {
    let mut cursor = index + 1;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    bytes.get(cursor) == Some(&b'"')
}

/// The extent of the raw string literal at `index`.
fn read_raw_string(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let mut cursor = index + 1;
    let mut hashes = 0;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }

    let start = cursor;
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"' {
            let mut end = cursor + 1;
            let mut found = 0;
            while found < hashes && bytes.get(end) == Some(&b'#') {
                found += 1;
                end += 1;
            }
            if found == hashes {
                return Some((start, end));
            }
        }
        cursor += 1;
    }

    None
}

/// The root of the repository.
fn repository_root() -> std::path::PathBuf {
    Path::new(MANIFEST_DIR)
        .parent()
        .and_then(Path::parent)
        .expect("the crate must live two levels below the repository root")
        .to_path_buf()
}

/// Parse the `serde` field names of every struct of the client.
fn parse_client_structs(source: &str) -> Vec<ClientStruct> {
    let lines: Vec<&str> = source.lines().collect();
    let mut structs = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let trimmed = lines[index].trim();
        let Some(rest) = trimmed.strip_prefix("pub struct ") else {
            index += 1;
            continue;
        };

        let name = rest
            .split(|character: char| character.is_whitespace() || character == '{')
            .next()
            .unwrap_or_default()
            .to_string();

        let mut fields: Vec<Field> = Vec::new();
        let mut has_default = false;
        let mut cursor = index + 1;
        while cursor < lines.len() {
            let line = lines[cursor].trim();
            if line.starts_with('}') {
                break;
            }
            if line.starts_with("#[serde(") {
                if line.contains("default") {
                    has_default = true;
                }
            } else if let Some(field) = line.strip_prefix("pub ")
                && let Some((name, _)) = field.split_once(':')
            {
                fields.push(Field {
                    name: name.trim().to_string(),
                    required: !has_default,
                });
                has_default = false;
            }
            cursor += 1;
        }

        structs.push(ClientStruct {
            name,
            fields,
            derives: derive_line(&lines, index),
        });
        index = cursor + 1;
    }

    structs
}

/// The `#[derive(…)]` line above a struct.
fn derive_line(lines: &[&str], index: usize) -> String {
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        let line = lines[cursor].trim();
        if line.starts_with("#[derive(") {
            return line.to_string();
        }
        if line.starts_with("///") || line.is_empty() {
            continue;
        }
        break;
    }
    String::new()
}

/// The keys the gateway produces for one handler.
fn gateway_handler(source: &str, handler: &str) -> Option<Gateway> {
    let body = function_body(source, handler)?;

    let mut top = BTreeSet::new();
    let mut payload: Vec<BTreeSet<String>> = Vec::new();
    let mut rows = BTreeSet::new();
    let mut nested: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (literal, is_response) in json_literals(body) {
        let Some(object) = parse_object(&literal) else {
            continue;
        };

        if is_response {
            top.extend(object.keys.iter().cloned());

            // A variant that carries no payload: an empty list or an error
            // message. It says nothing about the shape of the real answer.
            let degenerate = object.empty
                || object
                    .keys
                    .iter()
                    .all(|key| key == "error" || key == "usage");
            if !degenerate {
                payload.push(object.keys.iter().cloned().collect());
            }
        } else {
            rows.extend(object.keys.iter().cloned());
        }
        for (path, keys) in object.nested {
            nested.entry(path).or_default().extend(keys);
        }
    }

    Some(Gateway {
        top,
        payload,
        rows,
        nested,
    })
}

/// The body of a function of the gateway.
fn function_body<'a>(source: &'a str, handler: &str) -> Option<&'a str> {
    let marker = format!("fn {handler}(");
    let start = source.find(&marker)?;
    let open = start + source[start..].find('{')?;
    let close = matching_brace(source.as_bytes(), open)?;
    Some(&source[open..=close])
}

/// The index of the brace that closes the one at `open`.
fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let (_, next) = read_string(bytes, index)?;
                index = next;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }

    None
}

/// Every `json!` literal of a source: its text and whether it is the
/// top-level response (`Json(json!(…))`) rather than a row.
fn json_literals(source: &str) -> Vec<(String, bool)> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;

    while let Some(found) = source[index..].find("json!(") {
        let start = index + found;
        let open = start + "json!".len();
        let Some(close) = matching_paren(bytes, open) else {
            break;
        };

        let is_response = source[..start].trim_end().ends_with("Json(");
        literals.push((source[open + 1..close].to_string(), is_response));
        index = close + 1;
    }

    literals
}

/// The index of the parenthesis that closes the one at `open`.
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let (_, next) = read_string(bytes, index)?;
                index = next;
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }

    None
}

/// The parsed shape of a `json!` object literal.
struct Object {
    /// The keys of the object.
    keys: Vec<String>,
    /// The keys of the objects written inline, by their path.
    nested: Vec<(String, Vec<String>)>,
    /// `true` when every value of the object is an empty list.
    empty: bool,
    /// The index behind the closing brace.
    end: usize,
}

/// Parse the object of a `json!` literal: its keys and the keys of the
/// objects written inline.
fn parse_object(literal: &str) -> Option<Object> {
    let bytes = literal.as_bytes();
    let start = literal.find('{')?;
    parse_object_at(bytes, start)
}

/// Parse the object literal at `index`; returns its keys, the keys of its
/// inline nested objects and whether every value is an empty list.
fn parse_object_at(bytes: &[u8], index: usize) -> Option<Object> {
    if bytes.get(index) != Some(&b'{') {
        return None;
    }

    let mut cursor = index + 1;
    let mut keys = Vec::new();
    let mut nested = Vec::new();
    let mut empty = true;

    loop {
        cursor = skip_whitespace(bytes, cursor);
        match bytes.get(cursor) {
            None => return None,
            Some(b'}') => {
                return Some(Object {
                    keys,
                    nested,
                    empty,
                    end: cursor + 1,
                });
            }
            Some(b'"') => {}
            Some(_) => return None,
        }

        let (key, next) = read_string(bytes, cursor)?;
        cursor = skip_whitespace(bytes, next);
        if bytes.get(cursor) != Some(&b':') {
            return None;
        }
        cursor = skip_whitespace(bytes, cursor + 1);

        keys.push(key.clone());

        if bytes.get(cursor) == Some(&b'{') {
            let inner = parse_object_at(bytes, cursor)?;
            nested.push((key.clone(), inner.keys));
            for (path, keys) in inner.nested {
                nested.push((format!("{key}.{path}"), keys));
            }
            empty = false;
            cursor = inner.end;
        } else {
            let start = cursor;
            cursor = skip_value(bytes, cursor)?;
            let value = std::str::from_utf8(&bytes[start..cursor]).unwrap_or_default();
            if value.trim() != "[]" {
                empty = false;
            }
        }

        cursor = skip_whitespace(bytes, cursor);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => {
                return Some(Object {
                    keys,
                    nested,
                    empty,
                    end: cursor + 1,
                });
            }
            _ => return None,
        }
    }
}

/// Skip a value that is not an inline object.
fn skip_value(bytes: &[u8], mut index: usize) -> Option<usize> {
    let mut depth = 0usize;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let (_, next) = read_string(bytes, index)?;
                index = next;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
            }
            b',' if depth == 0 => return Some(index),
            _ => {}
        }
        index += 1;
    }

    None
}

/// Read a JSON string at `index`.
fn read_string(bytes: &[u8], index: usize) -> Option<(String, usize)> {
    if bytes.get(index) != Some(&b'"') {
        return None;
    }

    let mut out = String::new();
    let mut cursor = index + 1;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => {
                cursor += 1;
                let escaped = *bytes.get(cursor)?;
                out.push(match escaped {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'0' => '\0',
                    other => other as char,
                });
                cursor += 1;
            }
            b'"' => return Some((out, cursor + 1)),
            _ => {
                out.push(bytes[cursor] as char);
                cursor += 1;
            }
        }
    }

    None
}

/// Skip the whitespace at `index`.
fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}
