//! Cursor's browser PKCE polling flow and provider-scoped credential storage.
//! No desktop credential database or third-party runtime is used.
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_provider_api::ProviderError;
use codex_provider_api::Result;
use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::connect::bounded_body;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAuth {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

pub struct LoginChallenge {
    pub url: String,
    uuid: String,
    verifier: String,
}

impl LoginChallenge {
    pub fn new() -> Self {
        let mut random = [0u8; 32];
        rand::rng().fill_bytes(&mut random);
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
        let uuid = uuid::Uuid::new_v4().to_string();
        Self {
            url: format!(
                "https://cursor.com/loginDeepControl?challenge={challenge}&uuid={uuid}&mode=login&redirectTarget=cli"
            ),
            uuid,
            verifier,
        }
    }
}

impl Default for LoginChallenge {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct AuthStore {
    path: PathBuf,
    base_url: String,
    client: reqwest::Client,
    lock: Arc<Mutex<()>>,
}

impl AuthStore {
    pub fn new(path: PathBuf, base_url: &str) -> Result<Self> {
        let base_url = crate::validate_endpoint(base_url)?;
        Ok(Self {
            path,
            base_url,
            client: crate::http_client()?,
            lock: Arc::new(Mutex::new(())),
        })
    }

    /// The caller presents challenge.url in its existing login UI.
    pub async fn login(
        &self,
        challenge: LoginChallenge,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let auth = self.poll(challenge, cancellation).await?;
        self.save(auth).await
    }

    pub async fn poll(
        &self,
        challenge: LoginChallenge,
        cancellation: CancellationToken,
    ) -> Result<StoredAuth> {
        AuthClient {
            base_url: self.base_url.clone(),
            client: self.client.clone(),
        }
        .poll_auth(challenge, cancellation)
        .await
    }

    pub async fn load(&self) -> Result<StoredAuth> {
        let bytes = tokio::fs::read(&self.path).await.map_err(|_| {
            ProviderError::Authentication("Cursor credentials are missing or unreadable".into())
        })?;
        parse_auth(&bytes)
    }

    pub async fn save(&self, auth: StoredAuth) -> Result<()> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || save_atomic(&path, &auth))
            .await
            .map_err(|_| ProviderError::Authentication("Cursor credential write failed".into()))?
    }

    /// Refresh and durable write remain one critical section even if the caller
    /// drops its future after Cursor has rotated a refresh token.
    pub async fn token(&self, force_refresh: bool) -> Result<String> {
        let store = self.clone();
        tokio::spawn(async move {
            let _guard = store.lock.lock().await;
            let previous = store.load().await?;
            if !force_refresh && token_valid(&previous.access_token) {
                return Ok(previous.access_token);
            }
            let refresh = previous.refresh_token.ok_or_else(|| {
                ProviderError::Authentication(
                    "Cursor login required: token expired without refresh credentials".into(),
                )
            })?;
            let response = store
                .client
                .post(format!("{}/auth/refresh", store.base_url))
                .bearer_auth(&refresh)
                .header("content-type", "application/json")
                .body("{}")
                .send()
                .await
                .map_err(|_| ProviderError::Authentication("Cursor refresh failed".into()))?;
            if !response.status().is_success() {
                return Err(ProviderError::Authentication(
                    "Cursor refresh rejected; log in again".into(),
                ));
            }
            let mut auth = parse_auth(&bounded_body(response, &CancellationToken::new()).await?)?;
            if auth.refresh_token.is_none() {
                auth.refresh_token = Some(refresh);
            }
            let token = auth.access_token.clone();
            store.save(auth).await?;
            Ok(token)
        })
        .await
        .map_err(|_| ProviderError::Authentication("Cursor refresh task failed".into()))?
    }

    pub async fn logout(&self) -> Result<()> {
        let _guard = self.lock.lock().await;
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ProviderError::Authentication(
                "Cursor logout could not remove credentials".into(),
            )),
        }
    }
}

#[derive(Clone)]
pub struct AuthClient {
    base_url: String,
    client: reqwest::Client,
}

impl AuthClient {
    pub fn new(base_url: &str) -> Result<Self> {
        Ok(Self {
            base_url: crate::validate_endpoint(base_url)?,
            client: crate::http_client()?,
        })
    }
    /// Finish the browser flow without persisting it, for a host that owns a
    /// shared account pool and credential store.
    pub async fn poll_auth(
        &self,
        challenge: LoginChallenge,
        cancellation: CancellationToken,
    ) -> Result<StoredAuth> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
        loop {
            let request = self
                .client
                .post(format!("{}/auth/poll", self.base_url))
                .json(&serde_json::json!({"uuid": challenge.uuid, "verifier": challenge.verifier}));
            let response = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
                response = request.send() => response.map_err(|_| ProviderError::Transport("Cursor login poll failed".into()))?,
            };
            let status = response.status();
            // Existing CLI servers expose only GET; use the same single-use
            // verifier without printing it or including request URLs in errors.
            let response = if status == reqwest::StatusCode::NOT_FOUND {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
                    response = self.client.get(format!("{}/auth/poll", self.base_url))
                        .query(&[("uuid", &challenge.uuid), ("verifier", &challenge.verifier)]).send() =>
                        response.map_err(|_| ProviderError::Transport("Cursor login poll failed".into()))?,
                }
            } else {
                response
            };
            if response.status().is_success() {
                let auth = parse_auth(&bounded_body(response, &cancellation).await?)?;
                return Ok(auth);
            }
            if response.status() != reqwest::StatusCode::NOT_FOUND {
                return Err(ProviderError::Authentication(format!(
                    "Cursor login failed with HTTP {}",
                    response.status().as_u16()
                )));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ProviderError::Authentication(
                    "Cursor login timed out".into(),
                ));
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    }

    pub async fn refresh_auth(
        &self,
        refresh_token: String,
        cancellation: CancellationToken,
    ) -> Result<StoredAuth> {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
            result = refresh(&self.base_url, StoredAuth { access_token: String::new(), refresh_token: Some(refresh_token) }) => result,
        }
    }
}

/// An unverified expiry hint for proactive refresh; this never authenticates
/// a JWT locally. Upstream remains the authority for credential validity.
pub fn token_expires_at(token: &str) -> Option<u64> {
    let payload = URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    serde_json::from_slice::<serde_json::Value>(&payload)
        .ok()?
        .get("exp")?
        .as_u64()
}

/// Native refresh for hosts that own credential locking and writeback. The
/// host must persist the returned record before exposing it to another turn.
pub async fn refresh(base_url: &str, previous: StoredAuth) -> Result<StoredAuth> {
    let base_url = crate::validate_endpoint(base_url)?;
    let token = previous.refresh_token.ok_or_else(|| {
        ProviderError::Authentication("Cursor refresh credentials missing".into())
    })?;
    let response = crate::http_client()?
        .post(format!("{base_url}/auth/refresh"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .map_err(|_| ProviderError::Authentication("Cursor refresh failed".into()))?;
    if !response.status().is_success() {
        return Err(ProviderError::Authentication(
            "Cursor refresh rejected; log in again".into(),
        ));
    }
    let mut auth = parse_auth(&bounded_body(response, &CancellationToken::new()).await?)?;
    if auth.refresh_token.is_none() {
        auth.refresh_token = Some(token);
    }
    Ok(auth)
}

fn parse_auth(bytes: &[u8]) -> Result<StoredAuth> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| ProviderError::Authentication("invalid Cursor credential response".into()))?;
    let access_token = value
        .get("accessToken")
        .or_else(|| value.get("access_token"))
        .and_then(|token| token.as_str())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            ProviderError::Authentication("Cursor response missing access token".into())
        })?;
    let refresh_token = value
        .get("refreshToken")
        .or_else(|| value.get("refresh_token"))
        .and_then(|token| token.as_str())
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    Ok(StoredAuth {
        access_token: access_token.to_string(),
        refresh_token,
    })
}

fn token_valid(token: &str) -> bool {
    let Some(payload) = token.split('.').nth(1) else {
        return true;
    }; // API keys have no JWT expiry.
    let Ok(payload) = URL_SAFE_NO_PAD.decode(payload) else {
        return false;
    };
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return false;
    };
    let Some(expiry) = payload.get("exp").and_then(|v| v.as_u64()) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .unwrap_or(u64::MAX);
    expiry > now.saturating_add(60)
}

fn save_atomic(path: &Path, auth: &StoredAuth) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| ProviderError::Authentication("invalid Cursor credential path".into()))?;
    std::fs::create_dir_all(parent).map_err(|_| {
        ProviderError::Authentication("cannot create Cursor credential directory".into())
    })?;
    let temp = parent.join(format!(".cursor-auth-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(&serde_json::to_vec(auth)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|_| ProviderError::Authentication("cannot persist Cursor credentials".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_token_variants_without_debug_exposure() {
        let auth = parse_auth(br#"{"access_token":"access","refreshToken":"refresh"}"#).unwrap();
        assert_eq!(auth.access_token, "access");
        assert_eq!(auth.refresh_token.as_deref(), Some("refresh"));
        assert!(parse_auth(br#"{"accessToken":""}"#).is_err());
    }
    #[test]
    fn atomic_store_has_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cursor.json");
        save_atomic(
            &path,
            &StoredAuth {
                access_token: "access".into(),
                refresh_token: None,
            },
        )
        .unwrap();
        assert_eq!(
            parse_auth(&std::fs::read(&path).unwrap())
                .unwrap()
                .access_token,
            "access"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
