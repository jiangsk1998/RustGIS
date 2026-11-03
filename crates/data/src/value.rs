//! Attribute values and field definitions.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Integer,
    Double,
    Text,
    Boolean,
    /// An image attribute: the original bytes stay in the data source (see
    /// [`ImageValue`]); the attribute table shows a thumbnail and the viewer reads it when asked.
    Image,
}

impl FieldType {
    pub fn label(self) -> &'static str {
        match self {
            FieldType::Integer => "Long",
            FieldType::Double => "Double",
            FieldType::Text => "Text",
            FieldType::Boolean => "Boolean",
            FieldType::Image => "Image",
        }
    }
    pub fn is_numeric(self) -> bool {
        matches!(self, FieldType::Integer | FieldType::Double)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: FieldType,
    /// Display name (the field alias).
    #[serde(default)]
    pub alias: Option<String>,
}

impl Field {
    pub fn new(name: impl Into<String>, ty: FieldType) -> Self {
        Field { name: name.into(), ty, alias: None }
    }
    pub fn display(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

/// An image attribute's metadata. The original bytes aren't kept here: they stay in the data
/// source and are read when the thumbnail or the viewer asks for them, so a project holding a
/// large table of photos stays small. A thumbnail read so far is kept as a `data:` address; it
/// is what a project saves, so the table shows pictures without reaching the source again.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageValue {
    /// `image/jpeg`, `image/png`, …; empty until the image was read.
    #[serde(default)]
    pub mime: String,
    /// Pixel width and height; 0 until the image was read.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// The original image's byte count (0 when the source only knows its size).
    #[serde(default)]
    pub byte_len: u64,
    /// A thumbnail as `data:image/jpeg;base64,…`, once one was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,
}

impl ImageValue {
    pub fn new(mime: impl Into<String>, width: u32, height: u32, byte_len: u64) -> Self {
        ImageValue { mime: mime.into(), width, height, byte_len, thumbnail: None }
    }

    /// The image as its source knows it so far (its byte count, perhaps its size).
    pub fn sized(byte_len: u64) -> Self {
        ImageValue { byte_len, ..ImageValue::default() }
    }

    /// What the attribute table, the identify popup and `read_features` say: `image 1024×768 jpeg`,
    /// or `image (12 KB)` when only its size is known. Never the image's bytes.
    pub fn summary(&self) -> String {
        let format = match self.mime.as_str() {
            "image/jpeg" => Some("jpeg"),
            "image/png" => Some("png"),
            "image/webp" => Some("webp"),
            "image/gif" => Some("gif"),
            "image/tiff" => Some("tiff"),
            _ => None,
        };
        match (self.width > 0 && self.height > 0, format) {
            (true, Some(f)) => format!("image {}×{} {f}", self.width, self.height),
            (true, None) => format!("image {}×{}", self.width, self.height),
            (false, _) if self.byte_len > 0 => format!("image ({} KB)", self.byte_len.div_ceil(1024)),
            (false, _) => "image".to_string(),
        }
    }

    /// The metadata a value's JSON holds (never the bytes).
    pub fn meta_json(&self) -> serde_json::Value {
        serde_json::json!({"mime": self.mime, "width": self.width, "height": self.height, "byte_len": self.byte_len})
    }

    /// Heap bytes this value owns (for the chunk memory accounting).
    pub fn heap_bytes(&self) -> usize {
        self.mime.capacity() + self.thumbnail.as_ref().map_or(0, String::capacity)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Value {
    #[default]
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    Image(ImageValue),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) if f.is_finite() => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Text(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
            Value::Image(_) => None,
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
    /// Converts a JSON value to an attribute value.
    pub fn from_json(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => n.as_f64().map(Value::Float).unwrap_or(Value::Null),
            },
            serde_json::Value::String(s) => Value::Text(s.clone()),
            other => Value::Text(other.to_string()),
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Int(i) => (*i).into(),
            Value::Float(f) => serde_json::Number::from_f64(*f).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
            Value::Text(s) => s.clone().into(),
            Value::Bool(b) => (*b).into(),
            // Only what the image is, never its bytes: an external payload isn't accepted back.
            Value::Image(img) => serde_json::json!({"image": img.meta_json()}),
        }
    }
    /// Parses user text into a value of `ty` (empty → Null).
    pub fn parse(text: &str, ty: FieldType) -> Value {
        let t = text.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("<null>") {
            return Value::Null;
        }
        match ty {
            FieldType::Integer => {
                t.parse::<i64>().map(Value::Int).or_else(|_| t.parse::<f64>().map(|f| Value::Int(f as i64))).unwrap_or(Value::Null)
            }
            FieldType::Double => t.parse::<f64>().map(Value::Float).unwrap_or(Value::Null),
            FieldType::Boolean => Value::Bool(matches!(t.to_ascii_lowercase().as_str(), "true" | "yes" | "1" | "y")),
            FieldType::Text => Value::Text(t.to_string()),
            // Image fields are read-only: text typed into one isn't a picture.
            FieldType::Image => Value::Null,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "<Null>"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(v) => {
                if v.fract() == 0.0 && v.abs() < 1e15 {
                    write!(f, "{v:.0}")
                } else {
                    write!(f, "{}", (v * 1e6).round() / 1e6)
                }
            }
            Value::Text(s) => write!(f, "{s}"),
            Value::Bool(b) => write!(f, "{}", if *b { "True" } else { "False" }),
            Value::Image(img) => write!(f, "{}", img.summary()),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use std::cmp::Ordering;
        match (self.as_f64(), other.as_f64()) {
            (Some(a), Some(b)) if !matches!(self, Value::Text(_)) || !matches!(other, Value::Text(_)) => a.partial_cmp(&b),
            _ => match (self, other) {
                (Value::Null, Value::Null) => Some(Ordering::Equal),
                (Value::Null, _) => Some(Ordering::Less),
                (_, Value::Null) => Some(Ordering::Greater),
                _ => Some(self.to_string().to_lowercase().cmp(&other.to_string().to_lowercase())),
            },
        }
    }
}
