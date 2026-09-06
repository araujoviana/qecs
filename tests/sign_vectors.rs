//! Reference vectors from Huawei's "Constructing a Standard Request" doc plus an
//! independent HMAC vector (`printf 'hello' | openssl dgst -sha256 -hmac mysecret`).
use qecs::hwc::sign::*;

fn fixture() -> String {
    let parts = CanonicalParts {
        method: "GET",
        uri: "/v1/77b6a44cba5143ab91d13ab9a8ff44fd/vpcs",
        query: &[
            ("limit".into(), "2".into()),
            (
                "marker".into(),
                "13551d6b-755d-4757-b956-536f674975c0".into(),
            ),
        ],
        headers: &[
            ("Content-Type".into(), "application/json".into()),
            ("Host".into(), "service.region.example.com".into()),
            ("X-Sdk-Date".into(), "20191115T033655Z".into()),
        ],
        body: b"",
    };
    canonical_request(&parts)
}

#[test]
fn official_canonical_request_vector() {
    let expected = "GET\n\
/v1/77b6a44cba5143ab91d13ab9a8ff44fd/vpcs/\n\
limit=2&marker=13551d6b-755d-4757-b956-536f674975c0\n\
content-type:application/json\n\
host:service.region.example.com\n\
x-sdk-date:20191115T033655Z\n\
\n\
content-type;host;x-sdk-date\n\
e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(fixture(), expected);
}

#[test]
fn official_canonical_request_hash() {
    assert_eq!(
        hashed(&fixture()),
        "b25362e603ee30f4f25e7858e8a7160fd36e803bb2dfe206278659d71a9bcd7a"
    );
}

#[test]
fn official_string_to_sign_vector() {
    assert_eq!(
        string_to_sign("20191115T033655Z", &fixture()),
        "SDK-HMAC-SHA256\n20191115T033655Z\nb25362e603ee30f4f25e7858e8a7160fd36e803bb2dfe206278659d71a9bcd7a"
    );
}

#[test]
fn hmac_signature_is_stable_hex() {
    assert_eq!(
        signature("mysecret", "hello"),
        "f09399f0c446d84b31a080e57ec483392d41e6f512f3e7ada5027abbcd358c2a"
    );
}

#[test]
fn authorization_header_shape() {
    assert_eq!(
        authorization_header("MYAK", "host;x-sdk-date", "deadbeef"),
        "SDK-HMAC-SHA256 Access=MYAK, SignedHeaders=host;x-sdk-date, Signature=deadbeef"
    );
}

#[test]
fn empty_body_hash_is_the_known_sha256() {
    assert_eq!(
        hashed(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
