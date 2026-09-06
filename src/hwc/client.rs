//! A reqwest client that signs every request with SDK-HMAC-SHA256.
use crate::creds::Credentials;
use crate::hwc::sign::{self, CanonicalParts};
use reqwest::{
    Method,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::de::DeserializeOwned;

#[derive(Clone)]
pub struct SignedClient {
    http: reqwest::Client,
    creds: Credentials,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(c) => write!(f, "HWC API {} ({}): {}", self.status, c, self.message),
            None => write!(f, "HWC API {}: {}", self.status, self.message),
        }
    }
}
impl std::error::Error for ApiError {}

impl SignedClient {
    pub fn new(http: reqwest::Client, creds: Credentials) -> Self {
        SignedClient { http, creds }
    }

    pub(crate) fn signed_headers_for(
        &self,
        method: &Method,
        url: &str,
        body: Option<&[u8]>,
    ) -> HeaderMap {
        let parsed = reqwest::Url::parse(url).expect("valid url");
        let host = parsed.host_str().expect("url has host").to_string();
        let sdk_date = chrono::Utc::now().format(sign::DATE_FORMAT).to_string();
        let body = body.unwrap_or(b"");

        let mut hv: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-sdk-date".into(), sdk_date.clone()),
        ];
        if !body.is_empty() {
            hv.push(("content-type".into(), "application/json".into()));
        }
        if let Some(tok) = &self.creds.security_token {
            hv.push(("x-security-token".into(), tok.clone()));
        }

        let query: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let parts = CanonicalParts {
            method: method.as_str(),
            uri: parsed.path(),
            query: &query,
            headers: &hv,
            body,
        };
        let cr = sign::canonical_request(&parts);
        let sts = sign::string_to_sign(&sdk_date, &cr);
        let sig = sign::signature(&self.creds.sk, &sts);
        let (_c, signed) = sign::canonical_headers(&hv);
        let auth = sign::authorization_header(&self.creds.ak, &signed, &sig);

        let mut out = HeaderMap::new();
        out.insert("Host", HeaderValue::from_str(&host).unwrap());
        out.insert("X-Sdk-Date", HeaderValue::from_str(&sdk_date).unwrap());
        if !body.is_empty() {
            out.insert("Content-Type", HeaderValue::from_static("application/json"));
        }
        if let Some(tok) = &self.creds.security_token {
            out.insert("X-Security-Token", HeaderValue::from_str(tok).unwrap());
        }
        out.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(&auth).unwrap(),
        );
        out
    }

    pub async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, ApiError> {
        let raw = body.map(|b| serde_json::to_vec(b).unwrap());
        let headers = self.signed_headers_for(&method, url, raw.as_deref());
        let mut req = self.http.request(method, url).headers(headers);
        if let Some(r) = &raw {
            req = req.body(r.clone());
        }
        let resp = req.send().await.map_err(|e| ApiError {
            status: 0,
            code: None,
            message: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if (200..300).contains(&status) {
            if text.trim().is_empty() {
                serde_json::from_str::<T>("null").map_err(|e| ApiError {
                    status,
                    code: None,
                    message: format!("decoding empty response: {e}"),
                })
            } else {
                serde_json::from_str::<T>(&text).map_err(|e| ApiError {
                    status,
                    code: None,
                    message: format!("decoding response: {e}; body: {text}"),
                })
            }
        } else {
            let v: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let code = v
                .get("error_code")
                .and_then(|x| x.as_str())
                .or_else(|| v.pointer("/error/code").and_then(|x| x.as_str()))
                .map(String::from);
            let message = v
                .get("error_msg")
                .and_then(|x| x.as_str())
                .or_else(|| v.pointer("/error/message").and_then(|x| x.as_str()))
                .unwrap_or(&text)
                .to_string();
            Err(ApiError {
                status,
                code,
                message,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creds::Credentials;

    fn client(tok: Option<&str>) -> SignedClient {
        SignedClient::new(
            reqwest::Client::new(),
            Credentials {
                ak: "AK".into(),
                sk: "SK".into(),
                security_token: tok.map(String::from),
            },
        )
    }

    #[test]
    fn builds_authorization_and_date_headers() {
        let h = client(None).signed_headers_for(
            &Method::GET,
            "https://ecs.ap-southeast-3.myhuaweicloud.com/v1/proj/cloudservers/flavors",
            None,
        );
        assert!(h.get("X-Sdk-Date").is_some());
        let auth = h.get("Authorization").unwrap().to_str().unwrap();
        assert!(auth.starts_with("SDK-HMAC-SHA256 Access=AK, SignedHeaders="));
        assert!(auth.contains("host"));
        assert!(auth.contains("x-sdk-date"));
    }

    #[test]
    fn security_token_is_signed_when_present() {
        let h = client(Some("TOK")).signed_headers_for(
            &Method::GET,
            "https://ecs.x.myhuaweicloud.com/a/",
            None,
        );
        assert_eq!(h.get("X-Security-Token").unwrap(), "TOK");
        assert!(
            h.get("Authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("x-security-token")
        );
    }
}
