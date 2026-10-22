use hmac::{Hmac, Mac};
use http::{HeaderMap, HeaderValue, Method, Uri};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &[u8], method: &Method, uri: &Uri, timestamp: &str, body: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    let hash = hex(&Sha256::digest(body));
    let path = uri.path_and_query().map_or("/", |p| p.as_str());
    mac.update(format!("{method}\n{path}\n{timestamp}\n{hash}").as_bytes());
    mac
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to String");
    }
    out
}

pub(crate) fn sign(secret: &[u8], method: &Method, uri: &Uri, timestamp: &str, body: &[u8]) -> String {
    hex(&mac(secret, method, uri, timestamp, body).finalize().into_bytes())
}

pub(crate) fn verify(secret: &[u8], method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> bool {
    let Some(timestamp) = headers.get("x-sap-timestamp").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Ok(seconds) = timestamp.parse::<u64>() else {
        return false;
    };
    if crate::now().abs_diff(seconds) > 60 {
        return false;
    }
    let Some(signature) = headers.get("x-sap-signature").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if signature.len() != 64 {
        return false;
    }
    let mut bytes = [0; 32];
    for (i, pair) in signature.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        };
        let (Some(a), Some(b)) = (digit(pair[0]), digit(pair[1])) else {
            return false;
        };
        bytes[i] = a * 16 + b;
    }
    mac(secret, method, uri, timestamp, body).verify_slice(&bytes).is_ok()
}

pub(crate) fn header(value: &str) -> Result<HeaderValue, String> {
    HeaderValue::from_str(value).map_err(|_| "invalid authentication header".into())
}
