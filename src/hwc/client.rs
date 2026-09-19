use crate::creds::Credentials;
use crate::hwc::sign::{self, CanonicalParts};
use crate::telemetry::{HwcCall, Telemetry, current_phase};
use reqwest::{
    Method,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::de::DeserializeOwned;
use std::time::Instant;

#[derive(Clone)]
pub struct SignedClient {
    http: reqwest::Client,
    creds: Credentials,
    pub telemetry: Option<Telemetry>,
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
        SignedClient {
            http,
            creds,
            telemetry: None,
        }
    }

    pub fn creds(&self) -> &Credentials {
        &self.creds
    }

    pub fn with_telemetry(mut self, telemetry: Option<Telemetry>) -> Self {
        self.telemetry = telemetry;
        self
    }

    pub(crate) fn signed_headers_for_content(
        &self,
        method: &Method,
        url: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<HeaderMap, ApiError> {
        let parsed = reqwest::Url::parse(url).map_err(|e| ApiError {
            status: 0,
            code: None,
            message: format!("invalid url `{url}`: {e}"),
        })?;
        let host = parsed
            .host_str()
            .ok_or_else(|| ApiError {
                status: 0,
                code: None,
                message: format!("url `{url}` has no host"),
            })?
            .to_string();
        let sdk_date = chrono::Utc::now().format(sign::DATE_FORMAT).to_string();
        let body = body.unwrap_or(b"");

        let mut hv: Vec<(String, String)> = vec![
            ("host".into(), host.clone()),
            ("x-sdk-date".into(), sdk_date.clone()),
        ];
        if let Some(ct) = content_type {
            hv.push(("content-type".into(), ct.to_string()));
        } else if !body.is_empty() {
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
        out.insert(
            "Host",
            HeaderValue::from_str(&host).map_err(|e| ApiError {
                status: 0,
                code: None,
                message: format!("invalid Host header value: {e}"),
            })?,
        );
        out.insert(
            "X-Sdk-Date",
            HeaderValue::from_str(&sdk_date).map_err(|e| ApiError {
                status: 0,
                code: None,
                message: format!("invalid X-Sdk-Date header value: {e}"),
            })?,
        );
        if let Some(ct) = content_type {
            out.insert(
                "Content-Type",
                HeaderValue::from_str(ct).map_err(|e| ApiError {
                    status: 0,
                    code: None,
                    message: format!("invalid Content-Type header value: {e}"),
                })?,
            );
        } else if !body.is_empty() {
            out.insert("Content-Type", HeaderValue::from_static("application/json"));
        }
        if let Some(tok) = &self.creds.security_token {
            out.insert(
                "X-Security-Token",
                HeaderValue::from_str(tok).map_err(|e| ApiError {
                    status: 0,
                    code: None,
                    message: format!("invalid X-Security-Token header value: {e}"),
                })?,
            );
        }
        out.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_str(&auth).map_err(|e| ApiError {
                status: 0,
                code: None,
                message: format!("invalid Authorization header value: {e}"),
            })?,
        );
        Ok(out)
    }

    pub(crate) fn signed_headers_for(
        &self,
        method: &Method,
        url: &str,
        body: Option<&[u8]>,
    ) -> Result<HeaderMap, ApiError> {
        self.signed_headers_for_content(method, url, None, body)
    }

    pub async fn send_raw(
        &self,
        method: Method,
        url: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<(u16, String), ApiError> {
        let headers = self.signed_headers_for_content(&method, url, content_type, body)?;
        self.send_with_headers(method, url, headers, body).await
    }

    /// Send a request signed with AWS SigV4 headers instead of the HWC `SDK-HMAC-SHA256`
    /// scheme. Required for OBS bucket admin calls (create/list/delete) - OBS's
    /// S3-compatible API does not authenticate against the HWC scheme used for
    /// ECS/VPC/IAM/IMS in `send_raw`.
    pub async fn send_obs(
        &self,
        method: Method,
        url: &str,
        region: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<(u16, String), ApiError> {
        let body_bytes = body.unwrap_or(b"");
        let signed = crate::hwc::obs::sigv4_auth_headers(
            &self.creds,
            region,
            method.as_str(),
            url,
            body_bytes,
        );

        let mut headers = HeaderMap::new();
        for (k, v) in signed {
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| ApiError {
                status: 0,
                code: None,
                message: format!("invalid header name `{k}`: {e}"),
            })?;
            let value = HeaderValue::from_str(&v).map_err(|e| ApiError {
                status: 0,
                code: None,
                message: format!("invalid header value for `{k}`: {e}"),
            })?;
            headers.insert(name, value);
        }
        if let Some(ct) = content_type {
            headers.insert(
                "Content-Type",
                HeaderValue::from_str(ct).map_err(|e| ApiError {
                    status: 0,
                    code: None,
                    message: format!("invalid Content-Type header value: {e}"),
                })?,
            );
        }

        self.send_with_headers(method, url, headers, body).await
    }

    async fn send_with_headers(
        &self,
        method: Method,
        url: &str,
        headers: HeaderMap,
        body: Option<&[u8]>,
    ) -> Result<(u16, String), ApiError> {
        let t0 = Instant::now();
        let method_str = method.to_string();
        let parsed_url = reqwest::Url::parse(url).ok();
        let host = parsed_url
            .as_ref()
            .and_then(|u| u.host_str())
            .unwrap_or("")
            .to_string();
        let path = parsed_url
            .as_ref()
            .map(|u| u.path())
            .unwrap_or("")
            .to_string();

        let idempotent = is_idempotent(&method);
        let max_retries = if idempotent { 3 } else { 0 };
        let mut attempt = 0;

        loop {
            let mut req = self
                .http
                .request(method.clone(), url)
                .headers(headers.clone());
            if let Some(r) = body {
                req = req.body(r.to_vec());
            }
            let t_req = Instant::now();
            let send_res = req.send().await;
            let ttfb = t_req.elapsed();

            match send_res {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let should_retry =
                        idempotent && (status == 429 || (500..=599).contains(&status));
                    if should_retry && attempt < max_retries {
                        attempt += 1;
                        let nanos = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos())
                            .unwrap_or(0);
                        let jitter = (nanos % 50) as u64;
                        let backoff_ms = (100 * (1 << attempt)) + jitter;
                        log::debug!(
                            "Retrying {method_str} {url} after status {status} (attempt {attempt}/{max_retries}, backoff {backoff_ms}ms)"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }

                    let request_id = resp
                        .headers()
                        .get("x-request-id")
                        .or_else(|| resp.headers().get("x-openstack-request-id"))
                        .and_then(|v| v.to_str().ok())
                        .map(String::from);
                    let text = resp.text().await.unwrap_or_default();
                    let total = t0.elapsed();

                    if let Some(t) = &self.telemetry {
                        t.record_hwc(HwcCall {
                            method: method_str,
                            host,
                            path,
                            status,
                            request_id,
                            ttfb_ms: ttfb.as_millis() as u64,
                            total_ms: total.as_millis() as u64,
                            resp_bytes: text.len() as u64,
                            phase: current_phase().map(String::from),
                        });
                    }

                    return Ok((status, text));
                }
                Err(e) => {
                    if idempotent && attempt < max_retries {
                        attempt += 1;
                        let nanos = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.subsec_nanos())
                            .unwrap_or(0);
                        let jitter = (nanos % 50) as u64;
                        let backoff_ms = (100 * (1 << attempt)) + jitter;
                        log::debug!(
                            "Retrying {method_str} {url} after network error {e} (attempt {attempt}/{max_retries}, backoff {backoff_ms}ms)"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }

                    let total = t0.elapsed();
                    if let Some(t) = &self.telemetry {
                        t.record_hwc(HwcCall {
                            method: method_str,
                            host,
                            path,
                            status: 0,
                            request_id: None,
                            ttfb_ms: ttfb.as_millis() as u64,
                            total_ms: total.as_millis() as u64,
                            resp_bytes: 0,
                            phase: current_phase().map(String::from),
                        });
                    }
                    return Err(ApiError {
                        status: 0,
                        code: None,
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    pub async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, ApiError> {
        let raw = match body {
            Some(b) => Some(serde_json::to_vec(b).map_err(|e| ApiError {
                status: 0,
                code: None,
                message: format!("serializing request body: {e}"),
            })?),
            None => None,
        };
        let headers = self.signed_headers_for(&method, url, raw.as_deref())?;
        let (status, text) = self
            .send_with_headers(method, url, headers, raw.as_deref())
            .await?;

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

pub(crate) fn is_idempotent(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
    )
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
        let h = client(None)
            .signed_headers_for(
                &Method::GET,
                "https://ecs.ap-southeast-3.myhuaweicloud.com/v1/proj/cloudservers/flavors",
                None,
            )
            .unwrap();
        assert!(h.get("X-Sdk-Date").is_some());
        let auth = h.get("Authorization").unwrap().to_str().unwrap();
        assert!(auth.starts_with("SDK-HMAC-SHA256 Access=AK, SignedHeaders="));
        assert!(auth.contains("host"));
        assert!(auth.contains("x-sdk-date"));
    }

    #[test]
    fn security_token_is_signed_when_present() {
        let h = client(Some("TOK"))
            .signed_headers_for(&Method::GET, "https://ecs.x.myhuaweicloud.com/a/", None)
            .unwrap();
        assert_eq!(h.get("X-Security-Token").unwrap(), "TOK");
        assert!(
            h.get("Authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("x-security-token")
        );
    }

    #[test]
    fn invalid_url_returns_error_not_panic() {
        let c = client(None);
        let res = c.signed_headers_for(&Method::GET, "not-a-valid-url", None);
        assert!(res.is_err());
        assert!(res.unwrap_err().message.contains("invalid url"));
    }

    #[test]
    fn test_is_idempotent_classification() {
        assert!(is_idempotent(&Method::GET));
        assert!(is_idempotent(&Method::HEAD));
        assert!(is_idempotent(&Method::PUT));
        assert!(is_idempotent(&Method::DELETE));
        assert!(is_idempotent(&Method::OPTIONS));
        assert!(!is_idempotent(&Method::POST));
        assert!(!is_idempotent(&Method::PATCH));
    }
}
