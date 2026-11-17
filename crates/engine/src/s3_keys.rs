//! Opt-in storage of a saved S3 connection's access key in the desktop credential store
//! ([`rustgis_data::secrets`], supplied through [`crate::connections::DatabaseLogins`]).
//!
//! One opaque secret per saved connection, at the account `connection/<id>`: the JSON
//! `{"endpoint": <canonical endpoint, or null for AWS>, "accessKeyId": …, "secretAccessKey": …}`.
//! The endpoint travels with the keys so a connection whose address was changed outside the app
//! can never send them somewhere else. Nothing here prints, returns or logs a key: the decoded
//! payload has no `Debug`, and every failure is a fixed sentence.
//!
//! A temporary credential (one with a session token) is never stored, and the browser and the
//! command line have no store at all: they keep keys for the session, as before.

use std::sync::Arc;

use rustgis_data::secrets::{Secret, SecretStore};
use serde::{Deserialize, Serialize};

use crate::connections::{Connection, account};
use crate::render::rustgis_tiles::s3;

/// A stored value that can't be used (damaged, for another endpoint, or not S3 keys).
pub const UNUSABLE: &str = "Saved S3 keys could not be used. Enter the keys again.";
/// Nothing is saved for the connection.
pub const NONE_SAVED: &str = "No saved S3 keys. Enter accessKeyId and secretAccessKey.";
/// There is no credential store, or it refused.
pub const UNAVAILABLE: &str = "Saved S3 keys are unavailable. Enter the keys for this session.";
/// The login worked, the keys weren't kept.
pub const NOT_REMEMBERED: &str = "Keys are available for this session but were not remembered on this computer.";
/// A temporary credential isn't kept.
pub const TEMPORARY: &str = "Temporary credentials are session-only. Previously saved keys are unchanged.";
/// Saved keys couldn't be deleted, so what needed them gone didn't happen.
pub const NOT_FORGOTTEN: &str = "Saved S3 keys could not be forgotten. The connection was not changed.";

/// The most bytes a stored value may have.
const MAX_SECRET: usize = 8192;

#[derive(Serialize)]
struct Stored<'a> {
    endpoint: Option<&'a str>,
    #[serde(rename = "accessKeyId")]
    access_key_id: &'a str,
    #[serde(rename = "secretAccessKey")]
    secret_access_key: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Loaded {
    // AWS is explicit null, not an omitted identity field.
    #[serde(deserialize_with = "Deserialize::deserialize")]
    endpoint: Option<String>,
    #[serde(rename = "accessKeyId")]
    access_key_id: String,
    #[serde(rename = "secretAccessKey")]
    secret_access_key: String,
}

/// The canonical endpoint a saved connection's address stands for (`None`: AWS).
fn endpoint_of(c: &Connection) -> Result<Option<String>, &'static str> {
    s3::canonical(&c.address).map_err(|_| UNUSABLE)
}

fn encode(endpoint: Option<&str>, key: &str, secret: &str) -> Result<Secret, &'static str> {
    let text = serde_json::to_string(&Stored { endpoint, access_key_id: key, secret_access_key: secret }).map_err(|_| UNUSABLE)?;
    if text.len() > MAX_SECRET {
        return Err(UNUSABLE);
    }
    Ok(Secret::new(text))
}

fn decode(value: &Secret, endpoint: Option<&str>) -> Result<(String, String), &'static str> {
    let text = value.expose();
    if text.len() > MAX_SECRET {
        return Err(UNUSABLE);
    }
    // A decoding error would quote the text: its details are dropped.
    let l: Loaded = serde_json::from_str(text).map_err(|_| UNUSABLE)?;
    if l.endpoint.as_deref() != endpoint {
        return Err(UNUSABLE);
    }
    let credentials = s3::Credentials { access_key_id: l.access_key_id, secret_access_key: l.secret_access_key, session_token: None };
    s3::check_credentials(&credentials).map_err(|_| UNUSABLE)?;
    Ok((credentials.access_key_id, credentials.secret_access_key))
}

/// What reading a connection's saved keys gave.
pub enum Loading {
    /// The access key id and the secret access key.
    Keys(String, String),
    /// The store has none for it.
    Missing,
    /// There is no store, or it refused.
    Unavailable,
    /// What it holds can't be used.
    Unusable,
}

type Store<'a> = Option<&'a Arc<dyn SecretStore>>;

/// Reads the keys saved for `c` (a user action: this may ask the system to unlock its store).
pub fn load(store: Store<'_>, c: &Connection) -> Loading {
    let Some(store) = store else { return Loading::Unavailable };
    let value = match store.load(&account(&c.id)) {
        Ok(Some(v)) => v,
        Ok(None) => return Loading::Missing,
        Err(_) => return Loading::Unavailable,
    };
    let Ok(endpoint) = endpoint_of(c) else { return Loading::Unusable };
    match decode(&value, endpoint.as_deref()) {
        Ok((key, secret)) => Loading::Keys(key, secret),
        Err(_) => Loading::Unusable,
    }
}

/// Keeps the keys for `c`, replacing any earlier ones; whether they were kept.
pub fn save(store: Store<'_>, c: &Connection, key: &str, secret: &str) -> bool {
    let Some(store) = store else { return false };
    let Ok(endpoint) = endpoint_of(c) else { return false };
    let Ok(value) = encode(endpoint.as_deref(), key, secret) else { return false };
    store.save(&account(&c.id), &value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_saved_envelope_is_bound_to_its_endpoint_and_never_echoes_values() {
        let v = encode(Some("https://s3.example.org"), "AKIDEXAMPLE", "very-secret-value").unwrap();
        assert_eq!(decode(&v, Some("https://s3.example.org")).unwrap(), ("AKIDEXAMPLE".to_string(), "very-secret-value".to_string()));
        assert_eq!(decode(&v, None), Err(UNUSABLE));
        assert_eq!(decode(&v, Some("https://other.example.org")), Err(UNUSABLE));
        let aws = encode(None, "AKIDEXAMPLE", "very-secret-value").unwrap();
        assert!(decode(&aws, None).is_ok() && decode(&aws, Some("https://s3.example.org")).is_err());
        for bad in [
            "not json with very-secret-value",
            r#"{"accessKeyId":"A","secretAccessKey":"S"}"#,
            r#"{"endpoint":null,"accessKeyId":"A"}"#,
            r#"{"endpoint":null,"accessKeyId":"A","secretAccessKey":"S","extra":1}"#,
            r#"{"endpoint":null,"accessKeyId":"","secretAccessKey":"S"}"#,
            r#"{"endpoint":null,"accessKeyId":"has space","secretAccessKey":"S"}"#,
            r#"{"endpoint":null,"accessKeyId":7,"secretAccessKey":"S"}"#,
            "[]",
        ] {
            assert_eq!(decode(&Secret::new(bad), None), Err(UNUSABLE), "{bad}");
        }
        let long = format!(r#"{{"endpoint":null,"accessKeyId":"A","secretAccessKey":"{}"}}"#, "s".repeat(MAX_SECRET));
        assert_eq!(decode(&Secret::new(long), None), Err(UNUSABLE));
        assert_eq!(account("c12"), "connection/c12");
    }
}
