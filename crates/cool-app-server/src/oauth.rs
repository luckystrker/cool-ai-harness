//! Provider OAuth flows (P2.11): PKCE login plumbing for the public CLI
//! clients each vendor ships — Claude Code, OpenAI Codex and Gemini CLI —
//! plus token exchange, refresh and SecretKeyring persistence.
//!
//! Verified against the vendors' published clients:
//!
//! * **claude** — Claude Code's public client against
//!   `claude.ai/oauth/authorize` + `console.anthropic.com/v1/oauth/token`,
//!   scope `org:create_api_key user:profile user:inference`, PKCE S256 with
//!   the non-standard `state = verifier` echo. Subscription tokens require
//!   the `anthropic-beta: oauth-2025-04-20` header (wired in the driver).
//!   Consumer terms are written around Anthropic's own apps — off-label use
//!   that can stop working at any time; API keys stay the supported path.
//! * **chatgpt** — Codex CLI's public client against `auth.openai.com`:
//!   browser PKCE (`oauth/authorize` → `oauth/token`, callback
//!   `localhost:1455/auth/callback`) and the device flow
//!   (`api/accounts/deviceauth/{usercode,token}`). The resulting tokens
//!   authenticate Codex's `chatgpt.com` backend (Responses API), not the
//!   OpenAI platform `chat/completions` wire — so they are stored but the
//!   OpenAI-compatible driver refuses them with `oauth_wire_not_supported`.
//! * **gemini** — Gemini CLI's Google OAuth client against
//!   `accounts.google.com/o/oauth2/v2/auth` + `oauth2.googleapis.com/token`,
//!   scopes `cloud-platform` + `userinfo.{email,profile}`, loopback
//!   `http://127.0.0.1:{port}/oauth2callback` (Google allows any port). The
//!   public desktop client id/secret comes from `COOL_GOOGLE_CLIENT_ID` /
//!   `COOL_GOOGLE_CLIENT_SECRET` — secret scanning keeps them out of the
//!   repository.

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Static descriptor of one supported OAuth login.
#[derive(Clone, Debug)]
pub struct OAuthFlow {
    /// CLI/protocol name: `claude`, `chatgpt` or `gemini`.
    pub name: &'static str,
    /// Provider-row names this login attaches to (`find_or_create` matches).
    pub provider_names: &'static [&'static str],
    /// Canonical name used when auto-creating the provider row.
    pub provider_name: &'static str,
    pub api_base_url: &'static str,
    pub default_model: &'static str,
    authorize_url: &'static str,
    token_url: &'static str,
    client_id: Cow<'static, str>,
    /// Desktop-app client secret where the provider's public client needs
    /// one (Google) — supplied via env, never committed.
    client_secret: Option<Cow<'static, str>>,
    scope: &'static str,
    /// Extra authorize params (Claude's required `code=true`, Codex's
    /// `codex_cli_simplified_flow`/`originator`).
    extra_authorize: &'static [(&'static str, &'static str)],
    /// Anthropic's quirk: `state` must echo the PKCE verifier.
    state_is_verifier: bool,
    /// Fixed registered loopback port when the provider requires one.
    pub loopback_port: Option<u16>,
    /// Loopback path (Google uses `/oauth2callback`).
    pub loopback_path: &'static str,
    /// Manual paste-the-code redirect (`None` = loopback only).
    pub manual_redirect: Option<&'static str>,
    /// Codex-style device authorization flow endpoints, when supported,
    /// and the redirect URI the resulting code exchanges against.
    device_usercode_url: Option<&'static str>,
    device_token_url: Option<&'static str>,
    device_redirect_uri: Option<&'static str>,
}

const CLAUDE: OAuthFlow = OAuthFlow {
    name: "claude",
    provider_names: &["anthropic", "claude"],
    provider_name: "anthropic",
    api_base_url: "https://api.anthropic.com",
    default_model: "claude-sonnet-4-5",
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://console.anthropic.com/v1/oauth/token",
    client_id: Cow::Borrowed("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
    client_secret: None,
    scope: "org:create_api_key user:profile user:inference",
    extra_authorize: &[("code", "true")],
    state_is_verifier: true,
    loopback_port: Some(54545),
    loopback_path: "/callback",
    manual_redirect: Some("https://console.anthropic.com/oauth/code/callback"),
    device_usercode_url: None,
    device_token_url: None,
    device_redirect_uri: None,
};

const CHATGPT: OAuthFlow = OAuthFlow {
    name: "chatgpt",
    provider_names: &["openai", "chatgpt", "codex"],
    provider_name: "openai",
    api_base_url: "https://api.openai.com/v1/",
    default_model: "gpt-5-mini",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    client_id: Cow::Borrowed("app_EMoamEEZ73f0CkXaXp7hrann"),
    client_secret: None,
    scope: "openid profile email offline_access",
    extra_authorize: &[
        ("codex_cli_simplified_flow", "true"),
        ("originator", "cool"),
    ],
    state_is_verifier: false,
    loopback_port: Some(1455),
    loopback_path: "/auth/callback",
    manual_redirect: None,
    device_usercode_url: Some("https://auth.openai.com/api/accounts/deviceauth/usercode"),
    device_token_url: Some("https://auth.openai.com/api/accounts/deviceauth/token"),
    device_redirect_uri: Some("https://auth.openai.com/deviceauth/callback"),
};

fn gemini() -> OAuthFlow {
    OAuthFlow {
        name: "gemini",
        provider_names: &["gemini", "google"],
        provider_name: "gemini",
        api_base_url: "https://generativelanguage.googleapis.com",
        default_model: "gemini-2.5-flash",
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        // Google OAuth client credentials cannot be committed to the
        // repository (secret scanning blocks them); operators export the
        // public desktop client Google ships with gemini-cli. An unset
        // client id surfaces `oauth_client_unconfigured` at flow start.
        client_id: std::env::var("COOL_GOOGLE_CLIENT_ID")
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed("")),
        client_secret: std::env::var("COOL_GOOGLE_CLIENT_SECRET")
            .ok()
            .map(Cow::Owned),
        scope: "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile",
        extra_authorize: &[("access_type", "offline"), ("prompt", "consent")],
        state_is_verifier: false,
        loopback_port: None,
        loopback_path: "/oauth2callback",
        manual_redirect: None,
        device_usercode_url: None,
        device_token_url: None,
        device_redirect_uri: None,
    }
}

/// A flow is runnable only once its client credentials are configured.
/// Claude/OpenAI client ids ship in code (public client identifiers are
/// identifiers, not secrets); Google's pair is flagged by secret scanning
/// and comes from the environment instead.
pub fn flow_ready(flow: &OAuthFlow) -> Result<(), OAuthError> {
    if flow.client_id.is_empty() {
        return Err(OAuthError::new(
            "oauth_client_unconfigured",
            "Google OAuth needs COOL_GOOGLE_CLIENT_ID (and COOL_GOOGLE_CLIENT_SECRET for Google's token endpoint) — the public desktop client Google ships with gemini-cli; see .env.example",
        ));
    }
    Ok(())
}

/// All supported logins. Unknown names return `None` → the caller reports
/// `oauth_provider_unsupported` (spec: plumbing + clear error when a flow is
/// not usable instead of a guessed endpoint).
pub fn oauth_flow(name: &str) -> Option<OAuthFlow> {
    match name {
        "claude" | "anthropic" => Some(CLAUDE),
        "chatgpt" | "codex" => Some(CHATGPT),
        "gemini" | "google" => Some(gemini()),
        _ => None,
    }
}

impl OAuthFlow {
    /// The `state` for a started flow — Anthropic requires it to echo the
    /// PKCE verifier; everything else uses a random opaque value.
    pub fn state(&self, pkce: &Pkce) -> String {
        if self.state_is_verifier {
            pkce.verifier.clone()
        } else {
            random_state()
        }
    }
}

/// Errors from the OAuth plumbing — mapped to protocol `oauth_*` codes.
#[derive(Debug)]
pub struct OAuthError {
    pub code: &'static str,
    pub message: String,
}

impl OAuthError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for OAuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

/// PKCE verifier + S256 challenge. The verifier is the base64url encoding
/// of 32 random bytes — 43 chars, inside RFC 7636's 43–128 range.
#[derive(Clone, Debug)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn pkce_pair() -> Pkce {
    let mut entropy = uuid::Uuid::new_v4().as_bytes().to_vec();
    entropy.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entropy);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    Pkce {
        verifier,
        challenge,
    }
}

/// A random opaque state (used unless the provider wants `state = verifier`).
pub fn random_state() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(
        format!("{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()).as_bytes(),
    ))
}

/// OAuth handshake state kept between `oauth_start` and `oauth_complete`.
/// In-memory only — a restart restarts the login.
#[derive(Clone, Debug)]
pub struct PendingOAuth {
    pub flow: OAuthFlow,
    pub verifier: String,
    pub redirect_uri: String,
    pub created: Instant,
}

impl PendingOAuth {
    /// Started flows expire after 10 minutes.
    pub fn is_expired(&self) -> bool {
        self.created.elapsed() > Duration::from_secs(600)
    }
}

/// The redirect a started flow listens on: explicit `redirect_uri` wins,
/// else the provider's fixed loopback port, else ephemeral-loopback syntax
/// the caller substitutes its own bound port into.
pub fn redirect_uri(flow: OAuthFlow, requested: Option<&str>) -> String {
    if let Some(uri) = requested {
        return uri.to_owned();
    }
    if let Some(port) = flow.loopback_port {
        return format!("http://localhost:{port}{}", flow.loopback_path);
    }
    // Ephemeral port placeholder — the listener substitutes the real port.
    format!("http://127.0.0.1:0{}", flow.loopback_path)
}

/// Build the provider's authorization URL for a started flow.
pub fn authorize_url(flow: OAuthFlow, redirect_uri: &str, challenge: &str, state: &str) -> String {
    let mut pairs: Vec<(String, String)> = vec![
        ("client_id".to_owned(), flow.client_id.to_string()),
        ("response_type".to_owned(), "code".to_owned()),
        ("redirect_uri".to_owned(), redirect_uri.to_owned()),
        ("scope".to_owned(), flow.scope.to_owned()),
        ("code_challenge".to_owned(), challenge.to_owned()),
        ("code_challenge_method".to_owned(), "S256".to_owned()),
        // Anthropic requires `state` to echo the verifier; callers pass
        // `verifier` as `state` for flows with `state_is_verifier`.
        ("state".to_owned(), state.to_owned()),
    ];
    for (key, value) in flow.extra_authorize {
        pairs.push((key.to_string(), value.to_string()));
    }
    let encoded = pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encode(key.as_bytes()),
                percent_encode(value.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{}?{encoded}", flow.authorize_url)
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for byte in bytes {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Token bundle persisted (Fernet-encrypted) as `provider:{id}:oauth`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds when `access_token` expires, when the provider says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl OAuthTokens {
    /// True when expiry is known and passed (with a 60 s skew allowance).
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires| expires <= now_seconds() + 60)
    }
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

fn parse_tokens(body: &[u8], keep_refresh: Option<String>) -> Result<OAuthTokens, OAuthError> {
    let response: TokenResponse = serde_json::from_slice(body)
        .map_err(|error| OAuthError::new("oauth_token_invalid", error.to_string()))?;
    Ok(OAuthTokens {
        access_token: response.access_token,
        // Providers rotate refresh tokens; keep the fresh one when present.
        refresh_token: response.refresh_token.or(keep_refresh),
        expires_at: response.expires_in.map(|secs| now_seconds() + secs),
    })
}

async fn token_post(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<OAuthTokens, OAuthError> {
    let response = http
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|error| OAuthError::new("oauth_token_request_failed", error.to_string()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| OAuthError::new("oauth_token_request_failed", error.to_string()))?;
    if !status.is_success() {
        return Err(OAuthError::new(
            "oauth_token_rejected",
            format!("token endpoint returned {status}"),
        ));
    }
    parse_tokens(&body, None)
}

/// Exchange an authorization `code` for tokens (PKCE `code_verifier`).
pub async fn exchange_code(
    http: &reqwest::Client,
    flow: OAuthFlow,
    pending: &PendingOAuth,
    code: &str,
) -> Result<OAuthTokens, OAuthError> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("client_id", flow.client_id.as_ref()),
        ("code", code),
        ("redirect_uri", pending.redirect_uri.as_str()),
        ("code_verifier", pending.verifier.as_str()),
    ];
    if let Some(secret) = flow.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    token_post(http, flow.token_url, &form).await
}

/// Refresh an access token; keeps the previous refresh token when the
/// provider does not rotate one back.
pub async fn refresh_tokens(
    http: &reqwest::Client,
    flow: OAuthFlow,
    refresh_token: &str,
) -> Result<OAuthTokens, OAuthError> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("client_id", flow.client_id.as_ref()),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = flow.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let mut tokens = token_post(http, flow.token_url, &form).await?;
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_owned());
    }
    Ok(tokens)
}

/// Codex device flow: request the user code + polling handle.
pub struct DeviceAuth {
    pub device_auth_id: String,
    pub user_code: String,
    pub verification_url: String,
    pub interval_secs: u64,
}

/// `POST /api/accounts/deviceauth/usercode` — Codex device flow start.
pub async fn device_user_code(
    http: &reqwest::Client,
    flow: OAuthFlow,
) -> Result<DeviceAuth, OAuthError> {
    let url = flow
        .device_usercode_url
        .ok_or_else(|| OAuthError::new("oauth_device_unsupported", flow.name))?;
    let response = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&json!({"client_id": flow.client_id}))
        .send()
        .await
        .map_err(|error| OAuthError::new("oauth_device_request_failed", error.to_string()))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| OAuthError::new("oauth_device_request_failed", error.to_string()))?;
    if !status.is_success() {
        return Err(OAuthError::new(
            "oauth_device_rejected",
            format!("device authorization endpoint returned {status}"),
        ));
    }
    #[derive(Deserialize)]
    struct UserCodeResponse {
        device_auth_id: String,
        user_code: String,
        #[serde(default)]
        interval: Option<String>,
    }
    let parsed: UserCodeResponse = serde_json::from_slice(&body)
        .map_err(|error| OAuthError::new("oauth_device_invalid", error.to_string()))?;
    Ok(DeviceAuth {
        device_auth_id: parsed.device_auth_id,
        user_code: parsed.user_code.clone(),
        verification_url: "https://auth.openai.com/codex/device".to_owned(),
        interval_secs: parsed
            .interval
            .and_then(|value| value.parse().ok())
            .unwrap_or(5),
    })
}

/// One poll of the Codex device token endpoint: `Ok(None)` while the user
/// is still authorizing (403/404), `Ok(Some)` once issued.
pub async fn device_poll_once(
    http: &reqwest::Client,
    flow: OAuthFlow,
    device: &DeviceAuth,
) -> Result<Option<OAuthTokens>, OAuthError> {
    let url = flow
        .device_token_url
        .ok_or_else(|| OAuthError::new("oauth_device_unsupported", flow.name))?;
    let response = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "device_auth_id": device.device_auth_id,
            "user_code": device.user_code,
        }))
        .send()
        .await
        .map_err(|error| OAuthError::new("oauth_device_request_failed", error.to_string()))?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| OAuthError::new("oauth_device_request_failed", error.to_string()))?;
    if !status.is_success() {
        return Err(OAuthError::new(
            "oauth_device_rejected",
            format!("device token endpoint returned {status}"),
        ));
    }
    #[derive(Deserialize)]
    struct DeviceTokenResponse {
        // Codex returns {authorization_code, code_verifier} on success,
        // then the code exchanges against /oauth/token like a normal grant.
        #[serde(default)]
        authorization_code: Option<String>,
        #[serde(default)]
        code_verifier: Option<String>,
        #[serde(default)]
        access_token: Option<String>,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_in: Option<i64>,
    }
    let parsed: DeviceTokenResponse = serde_json::from_slice(&body)
        .map_err(|error| OAuthError::new("oauth_device_invalid", error.to_string()))?;
    if let Some(access_token) = parsed.access_token {
        return Ok(Some(OAuthTokens {
            access_token,
            refresh_token: parsed.refresh_token,
            expires_at: parsed.expires_in.map(|secs| now_seconds() + secs),
        }));
    }
    if let (Some(code), Some(verifier)) = (parsed.authorization_code, parsed.code_verifier) {
        // The device grant comes back as authorization_code + code_verifier
        // and exchanges against the standard token endpoint with the fixed
        // deviceauth/callback redirect.
        let redirect = flow
            .device_redirect_uri
            .ok_or_else(|| OAuthError::new("oauth_device_unsupported", flow.name))?;
        let form: Vec<(&str, &str)> = vec![
            ("grant_type", "authorization_code"),
            ("client_id", flow.client_id.as_ref()),
            ("code", code.as_str()),
            ("redirect_uri", redirect),
            ("code_verifier", verifier.as_str()),
        ];
        return token_post(http, flow.token_url, &form).await.map(Some);
    }
    Err(OAuthError::new(
        "oauth_device_invalid",
        "device token response carried neither tokens nor a code grant",
    ))
}

/// A `cool-agent` `AccessTokenSource` over the provider row: decrypts
/// `provider:{id}:oauth`, refreshes expired/401 tokens, re-encrypts.
pub struct ProviderTokenSource {
    flow: OAuthFlow,
    provider_id: i64,
    actor_id: String,
    store: Arc<cool_store::LegacyStore>,
    secrets: Arc<cool_security::SecretKeyring>,
    http: reqwest::Client,
    /// Cached decrypted tokens; refreshed lazily.
    cached: tokio::sync::Mutex<Option<OAuthTokens>>,
}

impl fmt::Debug for ProviderTokenSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderTokenSource")
            .field("flow", &self.flow.name)
            .field("provider_id", &self.provider_id)
            .finish_non_exhaustive()
    }
}

impl ProviderTokenSource {
    pub fn new(
        flow: OAuthFlow,
        provider_id: i64,
        actor_id: String,
        store: Arc<cool_store::LegacyStore>,
        secrets: Arc<cool_security::SecretKeyring>,
    ) -> Self {
        Self {
            flow,
            provider_id,
            actor_id,
            store,
            secrets,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            cached: tokio::sync::Mutex::new(None),
        }
    }

    fn load(&self) -> Result<OAuthTokens, cool_agent::ProviderError> {
        let provider = self
            .store
            .get_provider(&self.actor_id, self.provider_id)
            .map_err(|error| {
                cool_agent::ProviderError::new("oauth_store", error.to_string(), false)
            })?;
        let stored = provider.oauth_tokens_encrypted.ok_or_else(|| {
            cool_agent::ProviderError::new(
                "oauth_tokens_missing",
                "provider has no stored OAuth tokens",
                false,
            )
        })?;
        let plaintext = self.secrets.decrypt(&stored).map_err(|error| {
            cool_agent::ProviderError::new("oauth_tokens_undecryptable", error.to_string(), false)
        })?;
        serde_json::from_str(&plaintext).map_err(|error| {
            cool_agent::ProviderError::new("oauth_tokens_invalid", error.to_string(), false)
        })
    }

    fn persist(&self, tokens: &OAuthTokens) -> Result<(), cool_agent::ProviderError> {
        let plaintext = serde_json::to_string(tokens).map_err(|error| {
            cool_agent::ProviderError::new("oauth_tokens_invalid", error.to_string(), false)
        })?;
        let encrypted = self.secrets.encrypt(&plaintext).map_err(|error| {
            cool_agent::ProviderError::new("secret_encryption_failed", error.to_string(), false)
        })?;
        self.store
            .set_provider_oauth_tokens(&self.actor_id, self.provider_id, &encrypted)
            .map_err(|error| {
                cool_agent::ProviderError::new("oauth_store", error.to_string(), false)
            })?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl cool_agent::AccessTokenSource for ProviderTokenSource {
    async fn access_token(&self) -> Result<String, cool_agent::ProviderError> {
        let mut cached = self.cached.lock().await;
        let tokens = match cached.clone() {
            Some(tokens) => tokens,
            None => self.load()?,
        };
        if tokens.is_expired() {
            let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
                cool_agent::ProviderError::new(
                    "oauth_refresh_missing",
                    "expired access token and no refresh token — run `cool auth` again",
                    false,
                )
            })?;
            let refreshed = refresh_tokens(&self.http, self.flow.clone(), &refresh_token)
                .await
                .map_err(|error| {
                    cool_agent::ProviderError::new(error.code, error.message, false)
                })?;
            self.persist(&refreshed)?;
            *cached = Some(refreshed.clone());
            return Ok(refreshed.access_token);
        }
        *cached = Some(tokens.clone());
        Ok(tokens.access_token)
    }

    async fn refresh(&self) -> Result<String, cool_agent::ProviderError> {
        let refresh_token = {
            let cached = self.cached.lock().await.clone();
            let tokens = match cached {
                Some(tokens) => tokens,
                None => self.load()?,
            };
            tokens.refresh_token.ok_or_else(|| {
                cool_agent::ProviderError::new(
                    "oauth_refresh_missing",
                    "no refresh token — run `cool auth` again",
                    false,
                )
            })?
        };
        let refreshed = refresh_tokens(&self.http, self.flow.clone(), &refresh_token)
            .await
            .map_err(|error| cool_agent::ProviderError::new(error.code, error.message, false))?;
        self.persist(&refreshed)?;
        *self.cached.lock().await = Some(refreshed.clone());
        Ok(refreshed.access_token)
    }
}

/// Find the actor's provider row for an OAuth login (`provider_names`
/// match), or create it. Used by `providers.oauth_complete` and `cool auth`.
pub fn oauth_provider_row(
    store: &cool_store::LegacyStore,
    actor_id: &str,
    flow: OAuthFlow,
) -> Result<cool_store::domains::providers::Provider, cool_store::StoreError> {
    for provider in store.list_providers(actor_id, true)? {
        if flow
            .provider_names
            .iter()
            .any(|name| provider.name.eq_ignore_ascii_case(name))
            && provider.auth_kind == "oauth"
        {
            return Ok(provider);
        }
    }
    store.create_provider(
        actor_id,
        &cool_store::domains::providers::NewProvider {
            name: flow.provider_name.to_owned(),
            label: Some(format!("{} (OAuth)", flow.provider_name)),
            base_url: Some(flow.api_base_url.to_owned()),
            api_key_encrypted: None,
            default_model: Some(flow.default_model.to_owned()),
            is_active: true,
            is_subscription: true,
            is_fallback: false,
            chat_models: None,
            auth_kind: Some("oauth".to_owned()),
        },
    )
}

/// Serialize + encrypt the token bundle for `set_provider_oauth_tokens`.
pub fn encrypt_tokens(
    secrets: &cool_security::SecretKeyring,
    tokens: &OAuthTokens,
) -> Result<String, OAuthError> {
    let plaintext = serde_json::to_string(tokens)
        .map_err(|error| OAuthError::new("oauth_tokens_invalid", error.to_string()))?;
    secrets
        .encrypt(&plaintext)
        .map_err(|error| OAuthError::new("secret_encryption_failed", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_of_verifier() {
        let pkce = pkce_pair();
        assert_eq!(pkce.verifier.len(), 43);
        assert_eq!(
            pkce.challenge,
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(Sha256::digest(pkce.verifier.as_bytes()))
        );
    }

    #[test]
    fn claude_authorize_url_carries_required_params() {
        let flow = oauth_flow("claude").unwrap();
        let url = authorize_url(
            flow,
            "https://console.anthropic.com/oauth/code/callback",
            "challenge",
            "the-verifier",
        );
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        assert!(url.contains("code=true"));
        assert!(url.contains("code_challenge=challenge"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=org%3Acreate_api_key"));
        assert!(url.contains("state=the-verifier"));
    }

    #[test]
    fn gemini_authorize_url_uses_google_endpoints() {
        let flow = oauth_flow("gemini").unwrap();
        let url = authorize_url(flow, "http://127.0.0.1:9/oauth2callback", "c", "s");
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("auth%2Fcloud-platform"));
    }

    #[test]
    fn unknown_provider_is_unsupported() {
        assert!(oauth_flow("mistral").is_none());
    }

    #[test]
    fn token_parse_keeps_existing_refresh_token() {
        let tokens = parse_tokens(
            br#"{"access_token": "at", "expires_in": 3600}"#,
            Some("old-refresh".to_owned()),
        )
        .unwrap();
        assert_eq!(tokens.access_token, "at");
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
        assert!(!tokens.is_expired());
    }
}
