//! Parser for Prometheus text exposition samples.
//!
//! A sample line is `metric_name{label="value",...} value [timestamp_ms]`, the
//! form `/metrics` pages and remote dumps use. Each sample becomes one JSON
//! object: the metric name under `metric`, each label under its own name, the
//! value under `value`, and the time under the timestamp field as RFC 3339. A
//! sample without a timestamp has no time, as a JSON record without one. Values
//! that are NaN or infinite are kept as the strings `NaN`, `+Inf` and `-Inf`,
//! since JSON has no form for them.

use serde_json::{Number, Value};

use crate::error::{Error, Result};
use crate::schema::format_timestamp;

/// One sample's fields, in the order they are stored.
pub(crate) type Fields = Vec<(String, Value)>;

/// Returns true for lines the parser skips: comments (`# HELP`, `# TYPE`, and
/// any other `#` line) and blank lines.
pub(crate) fn is_comment(line: &[u8]) -> bool {
    line.iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_none_or(|&b| b == b'#')
}

/// Parses one sample line into its fields. `timestamp_field` names the key for
/// the time. It must differ from `metric` and from every label name.
pub(crate) fn parse_sample(line: &[u8], timestamp_field: &str) -> Result<Fields> {
    let text = std::str::from_utf8(line).map_err(|_| invalid("sample is not UTF-8"))?;
    let mut cur = Cursor { text, pos: 0 };

    cur.skip_spaces();
    let metric = cur.take_while(is_metric_char);
    if metric.is_empty() {
        return Err(invalid("sample has no metric name"));
    }
    let mut fields: Fields = vec![("metric".to_owned(), Value::String(metric.to_owned()))];

    if cur.peek() == Some('{') {
        cur.pos += 1;
        parse_labels(&mut cur, &mut fields)?;
    }

    cur.skip_spaces();
    let value_text = cur.take_while(|c| !c.is_whitespace());
    if value_text.is_empty() {
        return Err(invalid("sample has no value"));
    }
    let value = value_text
        .parse::<f64>()
        .map_err(|_| invalid("sample value is not a number"))?;
    fields.push(("value".to_owned(), value_json(value)));

    cur.skip_spaces();
    if !cur.at_end() {
        let ms_text = cur.take_while(|c| !c.is_whitespace());
        let ms: i64 = ms_text
            .parse()
            .map_err(|_| invalid("sample timestamp is not an integer"))?;
        cur.skip_spaces();
        if !cur.at_end() {
            return Err(invalid("unexpected text after the sample timestamp"));
        }
        let nanos = ms
            .checked_mul(1_000_000)
            .ok_or_else(|| invalid("sample timestamp is out of range"))?;
        fields.push((
            timestamp_field.to_owned(),
            Value::String(format_timestamp(nanos)),
        ));
    }

    let mut seen = std::collections::BTreeSet::new();
    for (name, _) in &fields {
        if !seen.insert(name.as_str()) {
            return Err(invalid(&format!("duplicate field {name}")));
        }
    }
    Ok(fields)
}

/// Reads the label list after its opening brace, up to and including the closing one.
fn parse_labels(cur: &mut Cursor<'_>, fields: &mut Fields) -> Result<()> {
    loop {
        cur.skip_spaces();
        if cur.peek() == Some('}') {
            cur.pos += 1;
            return Ok(());
        }
        let name = cur.take_while(is_label_char);
        if name.is_empty() {
            return Err(invalid("label name is missing"));
        }
        cur.skip_spaces();
        if cur.peek() != Some('=') {
            return Err(invalid("label has no value"));
        }
        cur.pos += 1;
        cur.skip_spaces();
        let value = cur.take_quoted()?;
        fields.push((name.to_owned(), Value::String(value)));

        cur.skip_spaces();
        match cur.peek() {
            Some(',') => cur.pos += 1,
            Some('}') => {
                cur.pos += 1;
                return Ok(());
            }
            _ => return Err(invalid("labels are not closed")),
        }
    }
}

/// A sample value as JSON. A finite value is a number. NaN and the infinities
/// are strings, spelled the way Prometheus spells them.
fn value_json(value: f64) -> Value {
    match Number::from_f64(value) {
        Some(number) => Value::Number(number),
        None if value.is_nan() => Value::String("NaN".to_owned()),
        None if value.is_sign_positive() => Value::String("+Inf".to_owned()),
        None => Value::String("-Inf".to_owned()),
    }
}

fn is_metric_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == ':'
}

fn is_label_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn invalid(detail: &str) -> Error {
    Error::InvalidRecord(detail.to_owned())
}

/// Position in one sample line.
struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn skip_spaces(&mut self) {
        while let Some(c) = self.peek() {
            if !c.is_whitespace() {
                break;
            }
            self.pos += c.len_utf8();
        }
    }

    /// Returns the longest run from the current position whose characters satisfy `keep`.
    fn take_while(&mut self, keep: impl Fn(char) -> bool) -> &'a str {
        let text: &'a str = self.text;
        let start = self.pos;
        while let Some(c) = self.peek() {
            if !keep(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        &text[start..self.pos]
    }

    /// Reads a double-quoted label value with `\\`, `\"` and `\n` escapes.
    fn take_quoted(&mut self) -> Result<String> {
        if self.peek() != Some('"') {
            return Err(invalid("label value is not quoted"));
        }
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(invalid("label value is not closed"));
            };
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let Some(escaped) = self.peek() else {
                        return Err(invalid("label value ends in a backslash"));
                    };
                    self.pos += escaped.len_utf8();
                    out.push(match escaped {
                        'n' => '\n',
                        '\\' => '\\',
                        '"' => '"',
                        _ => return Err(invalid("unknown escape in label value")),
                    });
                }
                _ => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(fields: &'a Fields, name: &str) -> Option<&'a Value> {
        fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    #[test]
    fn parses_metric_labels_value_and_timestamp() {
        let fields = parse_sample(
            br#"http_requests_total{method="get",code="200"} 1027 1395066363000"#,
            "timestamp",
        )
        .unwrap();
        assert_eq!(get(&fields, "metric").unwrap(), "http_requests_total");
        assert_eq!(get(&fields, "method").unwrap(), "get");
        assert_eq!(get(&fields, "code").unwrap(), "200");
        assert_eq!(get(&fields, "value").unwrap(), 1027.0);
        assert_eq!(get(&fields, "timestamp").unwrap(), "2014-03-17T14:26:03Z");
    }

    #[test]
    fn sample_without_labels_or_timestamp() {
        let fields = parse_sample(b"up 1", "timestamp").unwrap();
        assert_eq!(fields.len(), 2);
        assert!(get(&fields, "timestamp").is_none());
    }

    #[test]
    fn special_values_and_escapes() {
        let fields = parse_sample(br#"m{path="a\"b\\c\nd"} +Inf"#, "timestamp").unwrap();
        assert_eq!(get(&fields, "value").unwrap(), "+Inf");
        assert_eq!(get(&fields, "path").unwrap(), "a\"b\\c\nd");
        let nan = parse_sample(b"m NaN", "timestamp").unwrap();
        assert_eq!(get(&nan, "value").unwrap(), "NaN");
    }

    #[test]
    fn rejects_malformed_samples() {
        for bad in [
            &b""[..],
            b"{a=\"b\"} 1",
            b"m{a=b} 1",
            b"m{a=\"b\" 1",
            b"m 1 2 3",
            b"m abc",
            b"m 1 notatime",
            b"m{a=\"b\",a=\"c\"} 1",
        ] {
            assert!(
                parse_sample(bad, "timestamp").is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        assert!(is_comment(b"# HELP up Whether the target is up"));
        assert!(is_comment(b"   "));
        assert!(!is_comment(b"up 1"));
    }
}
