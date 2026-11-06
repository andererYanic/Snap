//! Minimaler S3-Client (PUT/DELETE) mit AWS-Signatur v4 – für Cloudflare R2 und andere
//! S3-kompatible Speicher. Nur HTTPS (außer Loopback für Tests); TLS über rustls mit den
//! Mozilla-Wurzelzertifikaten (webpki-roots).

use crate::api_bail;
use anyhow::{Context, Result};
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Zugangsdaten und Ziel. Das Geheimnis wird nie geloggt oder über die API zurückgegeben.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// z. B. `https://<konto>.r2.cloudflarestorage.com`
    pub endpoint: String,
    pub bucket: String,
    #[serde(default = "auto_region")]
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

fn auto_region() -> String {
    "auto".into()
}

impl std::fmt::Debug for S3Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Config")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"***")
            .finish()
    }
}

impl S3Config {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.endpoint).context("Ungültiger Endpunkt")?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            api_bail!(InvalidInput, "Endpunkt muss https:// verwenden.");
        }
        if url.path() != "/" || url.query().is_some() {
            api_bail!(InvalidInput, "Endpunkt ohne Pfad angeben, z. B. https://<konto>.r2.cloudflarestorage.com");
        }
        let ok = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        if !ok(&self.bucket, 63) || !ok(&self.access_key_id, 256) || !ok(&self.region, 32) {
            api_bail!(InvalidInput, "Ungültiger Bucket, Schlüssel-ID oder Region.");
        }
        if self.secret_access_key.is_empty() || self.secret_access_key.len() > 512 {
            api_bail!(InvalidInput, "Ungültiger geheimer Schlüssel.");
        }
        Ok(())
    }
}

fn hex(b: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(b)
}

fn sha256_hex(b: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, b).as_ref())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data).as_ref().to_vec()
}

/// AWS-Signaturschlüssel (Datum JJJJMMTT, Region, Dienst).
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Signatur v4 für eine Anfrage. `headers` müssen bereits kleingeschrieben und sortiert sein
/// und `host` sowie `x-amz-date` enthalten. Gibt (signierte Kopfnamen, Signatur) zurück.
#[allow(clippy::too_many_arguments)]
pub fn sign(
    secret: &str,
    region: &str,
    service: &str,
    method: &str,
    uri: &str,
    query: &str,
    headers: &[(&str, &str)],
    payload_hash: &str,
    amz_date: &str,
) -> (String, String) {
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
    let signed: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
    let signed = signed.join(";");
    let canonical = format!("{method}\n{uri}\n{query}\n{canonical_headers}\n{signed}\n{payload_hash}");
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let sig = hex(&hmac_sha256(&signing_key(secret, date, region, service), to_sign.as_bytes()));
    (signed, sig)
}

/// S3-Objektschlüssel URI-kodieren (Schrägstriche bleiben).
fn encode_key(key: &str) -> String {
    key.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub struct S3Client {
    cfg: S3Config,
    http: reqwest::Client,
}

impl S3Client {
    pub fn new(cfg: S3Config) -> Result<Self> {
        cfg.validate()?;
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self { cfg, http })
    }

    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<(reqwest::Body, u64, &str)>,
    ) -> Result<reqwest::Response> {
        let url = reqwest::Url::parse(&self.cfg.endpoint)?;
        let host = match url.port() {
            Some(p) => format!("{}:{p}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        let uri = format!("/{}/{}", self.cfg.bucket, encode_key(key));
        let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        // Inhalt nicht hashen (große, gestreamte Dateien); TLS schützt den Inhalt unterwegs.
        let payload = "UNSIGNED-PAYLOAD";
        let headers = [("host", host.as_str()), ("x-amz-content-sha256", payload), ("x-amz-date", amz_date.as_str())];
        let (signed, sig) = sign(
            &self.cfg.secret_access_key,
            &self.cfg.region,
            "s3",
            method.as_str(),
            &uri,
            "",
            &headers,
            payload,
            &amz_date,
        );
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}/{}/s3/aws4_request, SignedHeaders={signed}, Signature={sig}",
            self.cfg.access_key_id,
            &amz_date[..8],
            self.cfg.region
        );
        let mut req = self
            .http
            .request(method, format!("{}{}", self.cfg.endpoint.trim_end_matches('/'), uri))
            .header("authorization", auth)
            .header("x-amz-content-sha256", payload)
            .header("x-amz-date", amz_date);
        if let Some((b, len, ctype)) = body {
            req = req.header("content-length", len).header("content-type", ctype).body(b);
        }
        Ok(req.send().await?)
    }

    pub async fn put(&self, key: &str, body: reqwest::Body, len: u64, content_type: &str) -> Result<()> {
        let resp = self.send(reqwest::Method::PUT, key, Some((body, len, content_type))).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text: String = resp.text().await.unwrap_or_default().chars().take(300).collect();
            api_bail!(Unavailable, "Speicher lehnte das Hochladen ab ({status}): {text}");
        }
        Ok(())
    }

    pub async fn put_bytes(&self, key: &str, data: Vec<u8>, content_type: &str) -> Result<()> {
        let len = data.len() as u64;
        self.put(key, reqwest::Body::from(data), len, content_type).await
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let resp = self.send(reqwest::Method::DELETE, key, None).await?;
        // 404: schon weg – auch in Ordnung.
        if !resp.status().is_success() && resp.status() != reqwest::StatusCode::NOT_FOUND {
            api_bail!(Unavailable, "Speicher lehnte das Löschen ab ({})", resp.status());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offizielle Beispiele aus der AWS-Dokumentation (Signaturversion 4).
    #[test]
    fn aws_known_answers() {
        let secret = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
        assert_eq!(
            hex(&signing_key(secret, "20150830", "us-east-1", "iam")),
            "c4afb1cc5771d871763a393e44b703571b55cc28424d1a5e86da6ed3c154a4b9"
        );
        let (signed, sig) = sign(
            secret,
            "us-east-1",
            "iam",
            "GET",
            "/",
            "Action=ListUsers&Version=2010-05-08",
            &[
                ("content-type", "application/x-www-form-urlencoded; charset=utf-8"),
                ("host", "iam.amazonaws.com"),
                ("x-amz-date", "20150830T123600Z"),
            ],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "20150830T123600Z",
        );
        assert_eq!(signed, "content-type;host;x-amz-date");
        assert_eq!(sig, "5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7");
    }

    #[test]
    fn config_validation_and_secret_hidden() {
        let ok = S3Config {
            endpoint: "https://abc.r2.cloudflarestorage.com".into(),
            bucket: "snap-links".into(),
            region: "auto".into(),
            access_key_id: "AKID".into(),
            secret_access_key: "geheim".into(),
        };
        ok.validate().unwrap();
        assert!(!format!("{ok:?}").contains("geheim"));
        for bad in ["http://abc.example", "ftp://x", "https://abc.example/pfad", "kein url"] {
            let c = S3Config { endpoint: bad.into(), ..ok.clone() };
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(S3Config { endpoint: "http://127.0.0.1:9000".into(), ..ok.clone() }.validate().is_ok());
        assert!(S3Config { bucket: "../x".into(), ..ok.clone() }.validate().is_err());
        assert_eq!(encode_key("links/ab/data"), "links/ab/data");
        assert_eq!(encode_key("a b/ä"), "a%20b/%C3%A4");
    }
}
