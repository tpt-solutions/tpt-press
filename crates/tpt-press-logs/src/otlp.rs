//! Parser for OpenTelemetry log exports in OTLP JSON.
//!
//! Each input line is one `ExportLogsServiceRequest`, as the OpenTelemetry
//! file exporter writes it. One line can hold several log records, so the
//! parser returns one record per `logRecords` entry. A record becomes a JSON
//! object:
//!
//! - `timestamp`: the record's `timeUnixNano`, or its `observedTimeUnixNano` when
//!   the first is zero, as RFC 3339 under the timestamp field. If both are zero
//!   the record has no time.
//! - `severity` and `severityNumber`, when present.
//! - `body`, as a string, number or boolean. Other body types are kept as their
//!   OTLP JSON.
//! - `traceId` and `spanId`, when present.
//! - each record attribute as `attr.<key>`, and each resource attribute as
//!   `resource.<key>`. The prefixes keep attribute names from clashing with the
//!   fields above.
//!
//! Only logs are handled. Metrics and traces are refused.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::schema::format_timestamp;

/// One record's fields, in the order they are stored.
pub(crate) type Fields = Vec<(String, Value)>;

/// Parses one line into its records.
pub(crate) fn parse_request(line: &[u8], timestamp_field: &str) -> Result<Vec<Fields>> {
    let request: Value =
        serde_json::from_slice(line).map_err(|e| invalid(&format!("not JSON: {e}")))?;
    let resource_logs = request
        .get("resourceLogs")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid("not an OTLP logs request: expected a resourceLogs array (metrics and traces are not supported)")
        })?;

    let mut records = Vec::new();
    for resource in resource_logs {
        let resource_attrs = attributes(resource.pointer("/resource/attributes"), "resource.")?;
        let scopes = resource
            .get("scopeLogs")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for scope in scopes {
            let logs = scope
                .get("logRecords")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for log in logs {
                records.push(record(log, &resource_attrs, timestamp_field)?);
            }
        }
    }
    Ok(records)
}

fn record(log: &Value, resource_attrs: &Fields, timestamp_field: &str) -> Result<Fields> {
    let mut fields: Fields = Vec::new();

    let time = nanos_field(log, "timeUnixNano")?;
    let observed = nanos_field(log, "observedTimeUnixNano")?;
    if let Some(nanos) = time.or(observed).filter(|&n| n != 0) {
        fields.push((
            timestamp_field.to_owned(),
            Value::String(format_timestamp(nanos)),
        ));
    }

    if let Some(text) = log.get("severityText").and_then(Value::as_str) {
        if !text.is_empty() {
            fields.push(("severity".to_owned(), Value::String(text.to_owned())));
        }
    }
    if let Some(number) = log.get("severityNumber").and_then(Value::as_i64) {
        fields.push(("severityNumber".to_owned(), Value::from(number)));
    }
    if let Some(body) = log.get("body") {
        fields.push(("body".to_owned(), any_value(body)));
    }
    for key in ["traceId", "spanId"] {
        if let Some(id) = log.get(key).and_then(Value::as_str) {
            if !id.is_empty() {
                fields.push((key.to_owned(), Value::String(id.to_owned())));
            }
        }
    }
    fields.extend(attributes(log.get("attributes"), "attr.")?);
    fields.extend(resource_attrs.iter().cloned());

    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in &fields {
        if !seen.insert(name.as_str()) {
            return Err(invalid(&format!("duplicate field {name}")));
        }
    }
    Ok(fields)
}

/// Reads a nanosecond time that OTLP JSON writes as a string of digits.
fn nanos_field(log: &Value, key: &str) -> Result<Option<i64>> {
    let Some(value) = log.get(key) else {
        return Ok(None);
    };
    let text = match value {
        Value::String(text) => text.as_str(),
        Value::Number(number) => return number.as_i64().map(Some).ok_or_else(|| invalid(key)),
        Value::Null => return Ok(None),
        _ => return Err(invalid(&format!("{key} is not a number"))),
    };
    let nanos: u64 = text
        .parse()
        .map_err(|_| invalid(&format!("{key} is not a number of nanoseconds")))?;
    i64::try_from(nanos)
        .map(Some)
        .map_err(|_| invalid(&format!("{key} is out of range")))
}

/// Flattens an OTLP attribute list into fields named with `prefix`.
fn attributes(list: Option<&Value>, prefix: &str) -> Result<Fields> {
    let Some(list) = list else {
        return Ok(Vec::new());
    };
    let entries = list
        .as_array()
        .ok_or_else(|| invalid("attributes is not an array"))?;
    entries
        .iter()
        .map(|entry| {
            let key = entry
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("an attribute has no key"))?;
            let value = entry.get("value").map(any_value).unwrap_or(Value::Null);
            Ok((format!("{prefix}{key}"), value))
        })
        .collect()
}

/// Turns an OTLP `AnyValue` into the plain JSON value it holds where that is
/// simple: strings, numbers and booleans. Other kinds are kept as OTLP JSON.
fn any_value(value: &Value) -> Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    if let Some(text) = object.get("stringValue").and_then(Value::as_str) {
        return Value::String(text.to_owned());
    }
    if let Some(number) = object.get("intValue") {
        // OTLP JSON writes 64-bit integers as strings.
        if let Some(parsed) = number.as_str().and_then(|s| s.parse::<i64>().ok()) {
            return Value::from(parsed);
        }
        if let Some(parsed) = number.as_i64() {
            return Value::from(parsed);
        }
    }
    if let Some(number) = object.get("doubleValue") {
        if let Some(parsed) = number.as_f64() {
            return Value::from(parsed);
        }
    }
    if let Some(flag) = object.get("boolValue").and_then(Value::as_bool) {
        return Value::Bool(flag);
    }
    value.clone()
}

fn invalid(detail: &str) -> Error {
    Error::InvalidRecord(detail.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> &'static [u8] {
        br#"{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"checkout"}}]},"scopeLogs":[{"logRecords":[
          {"timeUnixNano":"1700000000123456789","severityText":"ERROR","severityNumber":17,
           "body":{"stringValue":"payment failed"},
           "attributes":[{"key":"retries","value":{"intValue":"3"}},{"key":"ok","value":{"boolValue":false}}],
           "traceId":"0af7651916cd43dd8448eb211c80319c"},
          {"observedTimeUnixNano":"1700000001000000000","body":{"stringValue":"retry"}}
        ]}]}]}"#
    }

    fn get<'a>(fields: &'a Fields, name: &str) -> Option<&'a Value> {
        fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    #[test]
    fn one_line_gives_one_record_per_log() {
        let records = parse_request(line(), "timestamp").unwrap();
        assert_eq!(records.len(), 2);

        let first = &records[0];
        assert_eq!(
            get(first, "timestamp").unwrap(),
            "2023-11-14T22:13:20.123456789Z"
        );
        assert_eq!(get(first, "severity").unwrap(), "ERROR");
        assert_eq!(get(first, "severityNumber").unwrap(), 17);
        assert_eq!(get(first, "body").unwrap(), "payment failed");
        assert_eq!(get(first, "attr.retries").unwrap(), 3);
        assert_eq!(get(first, "attr.ok").unwrap(), false);
        assert_eq!(get(first, "resource.service.name").unwrap(), "checkout");
    }

    #[test]
    fn observed_time_is_the_fallback() {
        let records = parse_request(line(), "timestamp").unwrap();
        assert_eq!(
            get(&records[1], "timestamp").unwrap(),
            "2023-11-14T22:13:21Z"
        );
        assert!(get(&records[1], "severity").is_none());
    }

    #[test]
    fn metrics_and_bad_json_are_refused() {
        assert!(parse_request(br#"{"resourceMetrics":[]}"#, "timestamp").is_err());
        assert!(parse_request(b"not json", "timestamp").is_err());
    }
}
