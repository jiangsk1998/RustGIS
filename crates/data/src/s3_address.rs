//! The address of a web or `s3://` object read through one saved S3 connection.
//!
//! A saved connection's login is kept for the session under its own runtime *binding* (see
//! `rustgis_tiles::s3`), and an address that must be read with that connection's keys (and no
//! other's) names it: `rustgis-s3:<binding>:<endpoint-byte-length>:<endpoint>:<inner-url>`. The
//! endpoint is the connection's canonical one (empty for AWS), the inner address is a web address
//! or an `s3://` one, kept verbatim. An empty binding is an address no connection has been chosen
//! for yet. The reserved [`LEGACY`] marker additionally retains an unknown endpoint while a choice
//! is pending. Neither can be a live login or fetch data. Nothing here is secret, so a saved
//! project may keep these addresses; `rustgis_tiles::s3` authorizes live bindings, not this parser.

/// Starts an address read through a saved S3 connection.
pub const PREFIX: &str = "rustgis-s3:";
/// Reserved unresolved marker for legacy sources whose endpoint is unknown. Never a live login.
pub const LEGACY: &str = "0000000000000000";
/// The longest address, in bytes.
pub const MAX_ADDRESS: usize = 8192;
/// The longest endpoint, in bytes.
pub const MAX_ENDPOINT: usize = 2048;
/// How many hexadecimal digits a binding has.
pub const BINDING_LEN: usize = 16;

const BAD: &str = "not a saved S3 address";
const TOO_LONG: &str = "the saved S3 address is too long";

/// The parts of an address read through a saved connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundS3<'a> {
    /// The login's runtime token; empty: no login chosen, [`LEGACY`]: unknown endpoint.
    pub binding: &'a str,
    /// The connection's canonical endpoint (empty: AWS).
    pub endpoint: &'a str,
    /// The address read: `http://`, `https://` or `s3://`.
    pub url: &'a str,
}

/// Whether an address starts as one read through a saved connection, well formed or not. A
/// malformed one is never a file on this computer: callers keep it blocked.
pub fn is_wrapper(address: &str) -> bool {
    address.starts_with(PREFIX)
}

/// Whether `binding` is empty (unresolved) or [`BINDING_LEN`] lower-case hexadecimal digits.
pub fn valid_binding(binding: &str) -> bool {
    binding.is_empty() || (binding.len() == BINDING_LEN && binding.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}

fn web_or_s3(url: &str) -> bool {
    ["https://", "http://", "s3://"].iter().any(|p| url.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p)))
}

/// The parts of `address` when it is one read through a saved connection, `None` when it isn't
/// (it doesn't start with [`PREFIX`]), and a fixed error (never part of the input) when it starts
/// so but is malformed: a bad binding, endpoint length or boundary, controls, a nested wrapper, or
/// an inner address that isn't a web or `s3://` one. Borrows; never allocates.
pub fn parse(address: &str) -> Result<Option<BoundS3<'_>>, &'static str> {
    let Some(rest) = address.strip_prefix(PREFIX) else { return Ok(None) };
    if address.len() > MAX_ADDRESS {
        return Err(TOO_LONG);
    }
    if address.chars().any(char::is_control) {
        return Err(BAD);
    }
    let (binding, rest) = rest.split_once(':').ok_or(BAD)?;
    if !valid_binding(binding) {
        return Err(BAD);
    }
    let (length, rest) = rest.split_once(':').ok_or(BAD)?;
    if length.is_empty() || length.len() > 4 || !length.bytes().all(|b| b.is_ascii_digit()) {
        return Err(BAD);
    }
    let length: usize = length.parse().map_err(|_| BAD)?;
    if length > MAX_ENDPOINT {
        return Err(BAD);
    }
    // Slicing at a byte offset that isn't a character boundary is `None`, not a panic.
    let endpoint = rest.get(..length).ok_or(BAD)?;
    let url = rest.get(length..).and_then(|r| r.strip_prefix(':')).ok_or(BAD)?;
    if binding == LEGACY && (!endpoint.is_empty() || !url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("s3://"))) {
        return Err(BAD);
    }
    if is_wrapper(url) || !web_or_s3(url) {
        return Err(BAD);
    }
    Ok(Some(BoundS3 { binding, endpoint, url }))
}

/// The address read through a saved connection with the given parts, checked as [`parse`] does.
pub fn bind(binding: &str, endpoint: &str, url: &str) -> Result<String, &'static str> {
    if !valid_binding(binding) || endpoint.len() > MAX_ENDPOINT {
        return Err(BAD);
    }
    let out = format!("{PREFIX}{binding}:{}:{endpoint}:{url}", endpoint.len());
    match parse(&out)? {
        Some(_) => Ok(out),
        None => Err(BAD),
    }
}

/// The address that is read: a valid wrapper's inner address, or any other address as it is
/// (including a malformed wrapper, which no caller may treat as a file).
pub fn inner(address: &str) -> &str {
    match parse(address) {
        Ok(Some(b)) => b.url,
        _ => address,
    }
}

/// `address` with its binding replaced (a wrapper), or `None` when it isn't a valid wrapper or
/// the new binding isn't one. The endpoint and inner address stay.
pub fn rebound(address: &str, binding: &str) -> Option<String> {
    let b = parse(address).ok().flatten()?;
    bind(binding, b.endpoint, b.url).ok()
}

/// The address a server is reached at, for an address that may be read through a saved connection
/// (as it is, or as the archive after a tile template's `@`): the wrapper's inner address, else
/// `address` itself.
pub fn reached(address: &str) -> &str {
    let wrapped = if is_wrapper(address) { Some(address) } else { address.split_once('@').map(|(_, a)| a).filter(|a| is_wrapper(a)) };
    wrapped.and_then(|a| parse(a).ok().flatten()).map_or(address, |b| b.url)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: &str = "0123456789abcdef";

    #[test]
    fn s3_address_round_trips_every_part() {
        let url = "https://minio.example:9000/bucket/a b+c%20d.fgb?list-type=2&prefix=x%2Fy";
        let a = bind(B, "https://minio.example:9000", url).unwrap();
        assert_eq!(a, format!("rustgis-s3:{B}:26:https://minio.example:9000:{url}"));
        let p = parse(&a).unwrap().unwrap();
        assert_eq!((p.binding, p.endpoint, p.url), (B, "https://minio.example:9000", url));
        assert_eq!(inner(&a), url);
        // AWS: an empty endpoint; unresolved: an empty binding.
        let aws = bind("", "", "s3://my-bucket/k.tif").unwrap();
        assert_eq!(aws, "rustgis-s3::0::s3://my-bucket/k.tif");
        let p = parse(&aws).unwrap().unwrap();
        assert_eq!((p.binding, p.endpoint, p.url), ("", "", "s3://my-bucket/k.tif"));
        assert_eq!(rebound(&aws, B).unwrap(), format!("rustgis-s3:{B}:0::s3://my-bucket/k.tif"));
        // Not a wrapper: left alone.
        assert_eq!(parse("https://x.org/a").unwrap(), None);
        assert_eq!(inner("https://x.org/a"), "https://x.org/a");
        assert!(is_wrapper("rustgis-s3:junk") && !is_wrapper("s3://b/k"));
    }

    #[test]
    fn s3_address_rejects_malformed_values_without_echoing_them() {
        let endpoint = "https://h.example";
        let good = format!("rustgis-s3:{B}:{}:{endpoint}:https://h.example/b/k", endpoint.len());
        assert!(parse(&good).unwrap().is_some());
        let bad = [
            // binding: upper case, short, long, non-hex
            format!("rustgis-s3:{}:17:{endpoint}:https://h.example/k", B.to_uppercase()),
            "rustgis-s3:abc:17:https://h.example:https://h.example/k".to_string(),
            format!("rustgis-s3:{B}0:17:{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{}:17:{endpoint}:https://h.example/k", "g".repeat(16)),
            // Unknown origin is only meaningful for a bare s3 object, never a known endpoint.
            format!("rustgis-s3:{LEGACY}:{}:{endpoint}:s3://b/k", endpoint.len()),
            format!("rustgis-s3:{LEGACY}:0::https://h.example/b/k"),
            // length: missing, signed, too long, past the end, off by one either way
            format!("rustgis-s3:{B}::{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{B}:+17:{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{B}:99999:{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{B}:2049:{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{B}:16:{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{B}:18:{endpoint}:https://h.example/k"),
            // nothing after the endpoint, or no separator
            format!("rustgis-s3:{B}:17:{endpoint}"),
            format!("rustgis-s3:{B}:17:{endpoint}https://h.example/k"),
            // inner: not a web or s3 address, nested wrappers, anonymous marker
            format!("rustgis-s3:{B}:17:{endpoint}:ftp://h.example/k"),
            format!("rustgis-s3:{B}:17:{endpoint}:/etc/passwd"),
            format!("rustgis-s3:{B}:17:{endpoint}:rustgis-s3:{B}:17:{endpoint}:https://h.example/k"),
            format!("rustgis-s3:{B}:17:{endpoint}:rustgis-anonymous:https://h.example/k"),
            // controls
            format!("rustgis-s3:{B}:17:{endpoint}:https://h.example/k\n"),
            format!("rustgis-s3:{B}:17:{endpoint}:https://h.example/\u{7}k"),
        ];
        for a in &bad {
            let e = parse(a).unwrap_err();
            assert!(!e.contains("h.example") && !e.contains("passwd"), "{e}");
        }
        // A length that cuts a multi-byte character is an error, not a panic.
        assert!(parse(&format!("rustgis-s3:{B}:2:é:https://h.example/k")).is_ok());
        assert!(parse(&format!("rustgis-s3:{B}:1:é:https://h.example/k")).is_err());
        // The overall bound, and the endpoint's.
        let long = format!("https://h.example/{}", "k".repeat(MAX_ADDRESS));
        assert!(bind(B, endpoint, &long).is_err() && parse(&format!("rustgis-s3:{B}:17:{endpoint}:{long}")).is_err());
        assert!(bind(B, &"e".repeat(MAX_ENDPOINT + 1), "https://h.example/k").is_err());
        assert!(bind("nope", endpoint, "https://h.example/k").is_err());
        // A malformed wrapper is never read as its own inner address.
        assert_eq!(inner("rustgis-s3:junk:https://h.example/k"), "rustgis-s3:junk:https://h.example/k");
    }
}
