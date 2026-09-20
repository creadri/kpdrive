//! Thin Proton API client: envelope handling, SRP login, 2FA, token refresh,
//! and the two account calls needed to derive the key passphrase.

use anyhow::{Context, Result, anyhow, bail};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD as B64;
use proton_crypto_account::keys::UserKeys;
use proton_crypto_account::salts::KeySecret;
use zeroize::Zeroizing;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

pub const BASE_URL: &str = "https://drive-api.proton.me/";
const ACCOUNT_URL: &str = "https://account.proton.me";
/// Client id Proton assigns to third-party Drive apps in the browser sign-in flow.
const FORK_CLIENT_ID: &str = "external-drive";
const APP_VERSION: &str = concat!("external-drive-kpdrive@", env!("CARGO_PKG_VERSION"));
const ACCEPT: &str = "application/vnd.protonmail.api+json";
const USER_AGENT: &str = concat!("kpdrive/", env!("CARGO_PKG_VERSION"), " (Linux; KDE Plasma)");

/// What we persist in the wallet between runs.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Session {
    pub username: String,
    pub uid: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Salted mailbox password hash that unlocks the user keys.
    pub key_secret: Vec<u8>,
}

#[derive(Deserialize)]
pub struct User {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "UsedSpace")]
    pub used_space: u64,
    #[serde(rename = "MaxSpace")]
    pub max_space: u64,
    #[serde(rename = "Keys")]
    pub keys: UserKeys,
}

pub struct Api {
    http: reqwest::Client,
    pub session: Option<Session>,
}

impl Api {
    pub fn new(session: Option<Session>) -> Self {
        let http = reqwest::Client::builder().gzip(true).user_agent(USER_AGENT).build().expect("reqwest client");
        Self { http, session }
    }

    pub async fn get<T: DeserializeOwned>(&mut self, path: &str) -> Result<T> {
        self.call(Method::GET, path, None).await
    }

    pub async fn post<T: DeserializeOwned>(&mut self, path: &str, body: &Value) -> Result<T> {
        self.call(Method::POST, path, Some(body)).await
    }

    /// Raw block download: authorised by the per-block storage token, no session headers.
    pub async fn fetch_block(&self, url: &str, token: &str) -> Result<Vec<u8>> {
        let resp = self.http.get(url).header("pm-storage-token", token).send().await.context("fetch block")?;
        if !resp.status().is_success() {
            bail!("block fetch failed: HTTP {}", resp.status());
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn put<T: DeserializeOwned>(&mut self, path: &str, body: &Value) -> Result<T> {
        self.call(Method::PUT, path, Some(body)).await
    }

    /// Raw block upload to a storage URL: multipart field "Block", storage token, no session headers.
    pub async fn post_block(&self, url: &str, token: &str, ciphertext: Vec<u8>) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(ciphertext).file_name("blob").mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("Block", part);
        let resp = self.http.post(url).header("pm-storage-token", token).multipart(form).send().await.context("upload block")?;
        let status = resp.status();
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        parse_envelope::<Value>("storage block", status, value).map(|_| ())
    }

    pub async fn delete<T: DeserializeOwned>(&mut self, path: &str) -> Result<T> {
        self.call(Method::DELETE, path, None).await
    }

    async fn call<T: DeserializeOwned>(&mut self, method: Method, path: &str, body: Option<&Value>) -> Result<T> {
        let (status, value) = self.send(method.clone(), path, body).await?;
        if status == StatusCode::UNAUTHORIZED && self.session.is_some() {
            self.refresh().await?;
            let (status, value) = self.send(method, path, body).await?;
            return parse_envelope(path, status, value);
        }
        parse_envelope(path, status, value)
    }

    async fn send(&self, method: Method, path: &str, body: Option<&Value>) -> Result<(StatusCode, Value)> {
        let started = std::time::Instant::now();
        let debug = std::env::var_os("KPDRIVE_DEBUG").is_some();
        let mut req = self
            .http
            .request(method.clone(), format!("{BASE_URL}{path}"))
            .header("x-pm-appversion", APP_VERSION)
            .header(reqwest::header::ACCEPT, ACCEPT);
        if let Some(s) = &self.session {
            req = req.header("x-pm-uid", &s.uid).bearer_auth(&s.access_token);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.with_context(|| format!("request {path}"))?;
        let status = resp.status();
        let value = resp.json().await.unwrap_or(Value::Null);
        if debug {
            eprintln!("api {method} {path} -> {} in {} ms", status.as_u16(), started.elapsed().as_millis());
        }
        Ok((status, value))
    }

    async fn refresh(&mut self) -> Result<()> {
        let s = self.session.as_ref().ok_or_else(|| anyhow!("no session"))?;
        let body = json!({
            "UID": s.uid,
            "RefreshToken": s.refresh_token,
            "ResponseType": "token",
            "GrantType": "refresh_token",
            "RedirectURI": "https://proton.me",
        });
        // No bearer on the refresh call: the access token is what's being replaced.
        let resp = self
            .http
            .post(format!("{BASE_URL}auth/v4/refresh"))
            .header("x-pm-appversion", APP_VERSION)
            .header(reqwest::header::ACCEPT, ACCEPT)
            .header("x-pm-uid", &s.uid)
            .json(&body)
            .send()
            .await
            .context("refresh session")?;
        let status = resp.status();
        let value: Value = resp.json().await.unwrap_or(Value::Null);
        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "AccessToken")]
            access_token: String,
            #[serde(rename = "RefreshToken")]
            refresh_token: String,
        }
        let r: R = parse_envelope("auth/v4/refresh", status, value).context("session expired, run `kpdrive login`")?;
        let s = self.session.as_mut().expect("checked above");
        s.access_token = r.access_token;
        s.refresh_token = r.refresh_token;
        Ok(())
    }

    /// Browser sign-in (session fork), the flow Proton's own CLI uses. The
    /// browser handles password, 2FA and captcha; the fork payload carries the
    /// key password. `show` gets the URL to open and the code the user must confirm.
    pub async fn login_via_browser(&mut self, show: impl FnOnce(&str, &str)) -> Result<()> {
        #[derive(Deserialize)]
        struct Init {
            #[serde(rename = "Selector")]
            selector: String,
            #[serde(rename = "UserCode")]
            user_code: String,
        }
        #[derive(Deserialize)]
        struct Status {
            #[serde(rename = "Payload")]
            payload: String,
            #[serde(rename = "UID")]
            uid: String,
            #[serde(rename = "AccessToken")]
            access_token: String,
            #[serde(rename = "RefreshToken")]
            refresh_token: String,
        }

        self.session = None;
        let init: Init = self.get("auth/v4/sessions/forks").await?;
        let key = Zeroizing::new(proton_crypto::generate_secure_random_bytes::<32>());
        show(&sign_in_url(&init.user_code, &key), &init.user_code);

        // Poll like the reference clients: 5s initial delay, 5s interval, 10 min cap.
        // 422 means "browser hasn't finished yet".
        let path = format!("auth/v4/sessions/forks/{}", init.selector);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let status: Status = loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let (http, value) = self.send(Method::GET, &path, None).await?;
            if http != StatusCode::UNPROCESSABLE_ENTITY {
                break parse_envelope(&path, http, value)?;
            }
            if std::time::Instant::now() > deadline {
                bail!("browser sign-in timed out after 10 minutes");
            }
        };

        let key_secret = decrypt_fork_key_password(&key, &status.payload)?;
        self.session = Some(Session {
            username: String::new(),
            uid: status.uid,
            access_token: status.access_token,
            refresh_token: status.refresh_token,
            key_secret: key_secret.as_bytes().to_vec(),
        });

        // Prove the key password by unlocking a user key, and learn the username.
        let user = self.user().await?;
        let unlocked = user.keys.unlock(&proton_crypto::new_pgp_provider(), &KeySecret::new(key_secret.as_bytes().to_vec()));
        if unlocked.unlocked_keys.is_empty() {
            bail!("fork payload key password does not unlock any user key");
        }
        self.session.as_mut().expect("set above").username = user.name;
        Ok(())
    }

    pub async fn user(&mut self) -> Result<User> {
        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "User")]
            user: User,
        }
        Ok(self.get::<R>("core/v4/users").await?.user)
    }

    pub fn key_secret(&self) -> Result<KeySecret> {
        let s = self.session.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        Ok(KeySecret::new(s.key_secret.clone()))
    }
}

fn sign_in_url(user_code: &str, key: &[u8; 32]) -> String {
    let payload = format!("0:{user_code}:{}:{FORK_CLIENT_ID}", B64.encode(key));
    let mut enc = String::new();
    for b in payload.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => enc.push(b as char),
            _ => enc.push_str(&format!("%{b:02X}")),
        }
    }
    format!("{ACCOUNT_URL}/desktop/login?app=drive&pv=3#payload={enc}")
}

/// Payload is base64(nonce[12] || ciphertext || tag[16]), AES-256-GCM, AAD "fork",
/// plaintext `{"keyPassword": "..."}`.
fn decrypt_fork_key_password(key: &[u8; 32], payload: &str) -> Result<Zeroizing<String>> {
    let blob = B64.decode(payload).context("fork payload base64")?;
    if blob.len() < 28 {
        bail!("fork payload too short");
    }
    let (nonce, ct) = blob.split_at(12);
    let nonce: &[u8; 12] = nonce.try_into().expect("split at 12");
    let plain = Aes256Gcm::new(key.into())
        .decrypt(nonce.into(), Payload { msg: ct, aad: b"fork" })
        .map_err(|_| anyhow!("fork payload failed to decrypt"))?;
    let v: Value = serde_json::from_slice(&plain).context("fork payload json")?;
    v.get("keyPassword")
        .and_then(Value::as_str)
        .map(|s| Zeroizing::new(s.to_owned()))
        .ok_or_else(|| anyhow!("fork payload has no keyPassword"))
}

/// A failed Proton API call. Carries the code so callers can tell apart the ones
/// that are ordinary answers, such as 2501 for "no such thing".
#[derive(Debug)]
pub struct ApiError {
    pub code: i64,
    pub status: u16,
    pub message: String,
    pub path: String,
}

/// "does not exist" — an answer, not always a failure.
pub const DOES_NOT_EXIST: i64 = 2501;

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: Proton API error {} (HTTP {}): {}", self.path, self.code, self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

/// The code of a failed call, when the failure came from the API at all.
pub fn api_code(error: &anyhow::Error) -> Option<i64> {
    error.downcast_ref::<ApiError>().map(|e| e.code)
}

/// Every Proton response is `{Code, Error?, ...}`; 1000/1001 mean success.
fn parse_envelope<T: DeserializeOwned>(path: &str, status: StatusCode, value: Value) -> Result<T> {
    let code = value.get("Code").and_then(Value::as_i64).unwrap_or(0);
    if !(status.is_success() && (code == 1000 || code == 1001)) {
        return Err(ApiError {
            code,
            status: status.as_u16(),
            message: value.get("Error").and_then(Value::as_str).unwrap_or("no error message").to_owned(),
            path: path.to_owned(),
        }
        .into());
    }
    serde_json::from_value(value).context("unexpected response shape")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope() {
        let ok: Value = parse_envelope("t", StatusCode::OK, json!({"Code": 1000, "X": 1})).unwrap();
        assert_eq!(ok["X"], 1);
        let err = parse_envelope::<Value>("t", StatusCode::UNPROCESSABLE_ENTITY, json!({"Code": 8002, "Error": "Incorrect login credentials"}))
            .unwrap_err();
        assert!(err.to_string().contains("8002"), "{err}");
        assert!(parse_envelope::<Value>("t", StatusCode::OK, json!({"Code": 2000})).is_err());
    }

    #[test]
    fn fork_payload() {
        let key = [7u8; 32];
        let nonce = [3u8; 12];
        let ct = Aes256Gcm::new((&key).into())
            .encrypt((&nonce).into(), Payload { msg: br#"{"keyPassword":"secret"}"#, aad: b"fork" })
            .unwrap();
        let blob = B64.encode([&nonce[..], &ct].concat());
        assert_eq!(decrypt_fork_key_password(&key, &blob).unwrap().as_str(), "secret");
        assert!(decrypt_fork_key_password(&[0u8; 32], &blob).is_err());
        let url = sign_in_url("ABCD1234", &key);
        assert!(url.starts_with("https://account.proton.me/desktop/login?app=drive&pv=3#payload=0%3AABCD1234%3A"), "{url}");
        assert!(url.ends_with("%3Aexternal-drive"));
    }
}
