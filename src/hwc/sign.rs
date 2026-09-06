//! Huawei Cloud SDK-HMAC-SHA256 request signing.
//! Reference: support.huaweicloud.com/intl/en-us/devg-apisign/api-sign-algorithm-002.html
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub const ALGORITHM: &str = "SDK-HMAC-SHA256";
pub const DATE_FORMAT: &str = "%Y%m%dT%H%M%SZ";

pub struct CanonicalParts<'a> {
    pub method: &'a str,
    pub uri: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
}

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";

fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if UNRESERVED.contains(b) {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn canonical_uri(raw: &str) -> String {
    let trimmed = raw.trim_start_matches('/').trim_end_matches('/');
    let mut path = if trimmed.is_empty() {
        String::from("/")
    } else {
        format!(
            "/{}",
            trimmed.split('/').map(pct).collect::<Vec<_>>().join("/")
        )
    };
    if !path.ends_with('/') {
        path.push('/');
    }
    path
}

pub fn canonical_query(pairs: &[(String, String)]) -> String {
    let mut p: Vec<(String, String)> = pairs.iter().map(|(k, v)| (pct(k), pct(v))).collect();
    p.sort();
    p.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn canonical_headers(headers: &[(String, String)]) -> (String, String) {
    let mut h: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    h.sort_by(|a, b| a.0.cmp(&b.0));
    let canonical = h.iter().map(|(k, v)| format!("{k}:{v}\n")).collect::<String>();
    let signed = h
        .iter()
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>()
        .join(";");
    (canonical, signed)
}

pub fn hashed(s: &str) -> String {
    hashed_payload(s.as_bytes())
}

pub fn hashed_payload(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hex::encode(hasher.finalize())
}

pub fn canonical_request(p: &CanonicalParts) -> String {
    let (canon_headers, signed_headers) = canonical_headers(p.headers);
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        p.method,
        canonical_uri(p.uri),
        canonical_query(p.query),
        canon_headers,
        signed_headers,
        hashed_payload(p.body),
    )
}

pub fn string_to_sign(sdk_date: &str, canonical_request: &str) -> String {
    format!("{ALGORITHM}\n{sdk_date}\n{}", hashed(canonical_request))
}

pub fn signature(sk: &str, string_to_sign: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(sk.as_bytes()).expect("hmac accepts any key length");
    mac.update(string_to_sign.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn authorization_header(ak: &str, signed_headers: &str, signature: &str) -> String {
    format!("{ALGORITHM} Access={ak}, SignedHeaders={signed_headers}, Signature={signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_gets_trailing_slash() {
        assert_eq!(canonical_uri("/v1/proj/vpcs"), "/v1/proj/vpcs/");
        assert_eq!(canonical_uri(""), "/");
        assert_eq!(canonical_uri("/"), "/");
    }

    #[test]
    fn query_is_sorted_and_encoded() {
        let q = vec![("b".into(), "2".into()), ("a".into(), "x y".into())];
        assert_eq!(canonical_query(&q), "a=x%20y&b=2");
    }

    #[test]
    fn headers_lowercased_sorted_signed_list() {
        let h = vec![
            ("X-Sdk-Date".into(), " 20191115T033655Z ".into()),
            ("Host".into(), "h".into()),
        ];
        let (canon, signed) = canonical_headers(&h);
        assert_eq!(canon, "host:h\nx-sdk-date:20191115T033655Z\n");
        assert_eq!(signed, "host;x-sdk-date");
    }
}
