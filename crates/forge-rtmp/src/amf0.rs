//! AMF0, the encoding of RTMP's commands and metadata: numbers, booleans,
//! strings, objects, ECMA arrays and null (the types a publisher sends and
//! a service answers with).

use crate::RtmpError;

/// An AMF0 value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Boolean(bool),
    String(String),
    /// Properties in order.
    Object(Vec<(String, Value)>),
    Null,
    Undefined,
    /// An associative array: properties in order, as `onMetaData` is sent.
    EcmaArray(Vec<(String, Value)>),
    StrictArray(Vec<Value>),
}

impl Value {
    pub fn str(s: impl Into<String>) -> Self {
        Value::String(s.into())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A property of an object or ECMA array.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(props) | Value::EcmaArray(props) => {
                props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }
}

const NUMBER: u8 = 0x00;
const BOOLEAN: u8 = 0x01;
const STRING: u8 = 0x02;
const OBJECT: u8 = 0x03;
const NULL: u8 = 0x05;
const UNDEFINED: u8 = 0x06;
const ECMA_ARRAY: u8 = 0x08;
const OBJECT_END: u8 = 0x09;
const STRICT_ARRAY: u8 = 0x0A;
const LONG_STRING: u8 = 0x0C;

fn write_key(out: &mut Vec<u8>, key: &str) {
    out.extend_from_slice(&(key.len() as u16).to_be_bytes());
    out.extend_from_slice(key.as_bytes());
}

fn write_props(out: &mut Vec<u8>, props: &[(String, Value)]) {
    for (k, v) in props {
        write_key(out, k);
        encode_into(out, v);
    }
    out.extend_from_slice(&[0, 0, OBJECT_END]);
}

/// Append `value`'s encoding to `out`.
pub fn encode_into(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Number(n) => {
            out.push(NUMBER);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Value::Boolean(b) => {
            out.push(BOOLEAN);
            out.push(*b as u8);
        }
        Value::String(s) if s.len() <= u16::MAX as usize => {
            out.push(STRING);
            write_key(out, s);
        }
        Value::String(s) => {
            out.push(LONG_STRING);
            out.extend_from_slice(&(s.len() as u32).to_be_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Object(props) => {
            out.push(OBJECT);
            write_props(out, props);
        }
        Value::Null => out.push(NULL),
        Value::Undefined => out.push(UNDEFINED),
        Value::EcmaArray(props) => {
            out.push(ECMA_ARRAY);
            out.extend_from_slice(&(props.len() as u32).to_be_bytes());
            write_props(out, props);
        }
        Value::StrictArray(items) => {
            out.push(STRICT_ARRAY);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            for v in items {
                encode_into(out, v);
            }
        }
    }
}

/// Values one after another, as a command's body is.
pub fn encode(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        encode_into(&mut out, v);
    }
    out
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], RtmpError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.data.len())
            .ok_or_else(|| RtmpError::Protocol("AMF0 value runs past its message".into()))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16, RtmpError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, RtmpError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn string(&mut self, len: usize) -> Result<String, RtmpError> {
        Ok(String::from_utf8_lossy(self.take(len)?).into_owned())
    }

    fn props(&mut self, depth: u32) -> Result<Vec<(String, Value)>, RtmpError> {
        let mut props = Vec::new();
        loop {
            let len = self.u16()? as usize;
            if len == 0 {
                // The end marker; some encoders leave it off at the very end.
                if self.pos < self.data.len() && self.data[self.pos] == OBJECT_END {
                    self.pos += 1;
                }
                return Ok(props);
            }
            let key = self.string(len)?;
            props.push((key, self.value(depth + 1)?));
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value, RtmpError> {
        if depth > 16 {
            return Err(RtmpError::Protocol("AMF0 nested too deep".into()));
        }
        let marker = self.take(1)?[0];
        Ok(match marker {
            NUMBER => {
                let b = self.take(8)?;
                Value::Number(f64::from_be_bytes(b.try_into().expect("eight bytes")))
            }
            BOOLEAN => Value::Boolean(self.take(1)?[0] != 0),
            STRING => {
                let len = self.u16()? as usize;
                Value::String(self.string(len)?)
            }
            LONG_STRING => {
                let len = self.u32()? as usize;
                Value::String(self.string(len)?)
            }
            OBJECT => Value::Object(self.props(depth)?),
            NULL => Value::Null,
            UNDEFINED => Value::Undefined,
            ECMA_ARRAY => {
                let _count = self.u32()?;
                Value::EcmaArray(self.props(depth)?)
            }
            STRICT_ARRAY => {
                let count = self.u32()? as usize;
                let mut items = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Value::StrictArray(items)
            }
            other => {
                return Err(RtmpError::Protocol(format!(
                    "AMF0 type {other:#04x} is not one this reads"
                )))
            }
        })
    }
}

/// Every value in `data`.
pub fn decode(data: &[u8]) -> Result<Vec<Value>, RtmpError> {
    let mut r = Reader { data, pos: 0 };
    let mut out = Vec::new();
    while r.pos < data.len() {
        out.push(r.value(0)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_round_trips() {
        let values = vec![
            Value::str("connect"),
            Value::Number(1.0),
            Value::Object(vec![
                ("app".into(), Value::str("live2")),
                ("fpad".into(), Value::Boolean(false)),
            ]),
            Value::Null,
            Value::EcmaArray(vec![("width".into(), Value::Number(1280.0))]),
            Value::StrictArray(vec![Value::Undefined]),
        ];
        let bytes = encode(&values);
        assert_eq!(&bytes[..3], &[STRING, 0, 7], "a string is marked and sized");
        assert_eq!(decode(&bytes).unwrap(), values);
        assert_eq!(values[2].get("app").and_then(Value::as_str), Some("live2"));
    }

    #[test]
    fn a_short_value_is_an_error_not_a_panic() {
        let bytes = encode(&[Value::str("onStatus")]);
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        assert!(decode(&[0x42]).is_err());
    }
}
