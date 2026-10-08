//! Google OAuth for installed apps: loopback redirect + PKCE.
//!
//! The refresh token is stored in the OS credential store (Windows Credential
//! Manager / macOS Keychain) via `keyring`; access tokens live only in memory.
//! The consent page opens in the user's default browser; the app never embeds
//! a web view.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::GoogleConfig;
use crate::net::Http;

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/youtube.readonly";
/// Credential-store service name for the normal install.
pub const DEFAULT_KEYRING_SERVICE: &str = "yt-lite";
const KEYRING_USER: &str = "google-refresh-token";

#[derive(Debug)]
pub struct NotSignedIn;

impl std::fmt::Display for NotSignedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not signed in")
    }
}

impl std::error::Error for NotSignedIn {}

pub struct Auth {
    google: GoogleConfig,
    /// Credential-store service; separate installs (portable / test homes)
    /// use their own so they never read each other's tokens.
    keyring_service: String,
    http: Http,
    access: Mutex<Option<(String, Instant)>>,
    /// The refresh token, read from the credential store at most once per run.
    /// (On macOS every read of a Keychain item can prompt the user.)
    refresh: Mutex<Option<Option<String>>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
}

impl Auth {
    pub fn new(google: GoogleConfig, http: Http, keyring_service: impl Into<String>) -> Self {
        Self {
            google,
            keyring_service: keyring_service.into(),
            http,
            access: Mutex::new(None),
            refresh: Mutex::new(None),
        }
    }

    fn entry(&self) -> Result<CredentialEntry> {
        open_entry(&self.keyring_service, KEYRING_USER).context("opening credential store")
    }

    /// `None` if the credential store can save tokens, otherwise why not.
    /// Writes and deletes a probe entry, so front-ends can warn before a
    /// sign-in that couldn't be saved.
    pub fn credential_store_problem(&self) -> Option<String> {
        let probe = || -> Result<()> {
            let entry = open_entry(&self.keyring_service, "store-probe").context("opening credential store")?;
            entry.set_password("ok").context("writing to credential store")?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(anyhow!(e).context("deleting from credential store")),
            }
        };
        probe().err().map(|e| format!("{e:#}"))
    }

    fn refresh_token(&self) -> Result<Option<String>> {
        let mut cached = self.refresh.lock().unwrap();
        if let Some(token) = cached.as_ref() {
            return Ok(token.clone());
        }
        let token = self.read_refresh_token()?;
        *cached = Some(token.clone());
        Ok(token)
    }

    fn read_refresh_token(&self) -> Result<Option<String>> {
        match self.entry()?.get_password() {
            Ok(t) => Ok(Some(t)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!(e).context("reading credential store")),
        }
    }

    pub fn is_signed_in(&self) -> bool {
        matches!(self.refresh_token(), Ok(Some(_)))
    }

    pub fn sign_out(&self) -> Result<()> {
        *self.access.lock().unwrap() = None;
        *self.refresh.lock().unwrap() = Some(None);
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Drops the cached access token (e.g. after a 401).
    pub fn invalidate(&self) {
        *self.access.lock().unwrap() = None;
    }

    /// Returns a valid access token, refreshing it if needed. Blocking.
    pub fn access_token(&self) -> Result<String> {
        if let Some((tok, expires)) = self.access.lock().unwrap().as_ref()
            && Instant::now() + Duration::from_secs(60) < *expires
        {
            return Ok(tok.clone());
        }
        let refresh = self.refresh_token()?.ok_or(NotSignedIn)?;
        let resp: TokenResponse = self
            .http
            .post_form(
                TOKEN_URL,
                &[
                    ("client_id", &self.google.client_id),
                    ("client_secret", &self.google.client_secret),
                    ("refresh_token", &refresh),
                    ("grant_type", "refresh_token"),
                ],
            )
            .map_err(|e| {
                // invalid_grant: revoked, or expired (7-day limit while the
                // OAuth app is in "Testing" status). Force a new sign-in.
                if e.to_string().contains("expired or revoked")
                    || e.to_string().contains("invalid_grant")
                {
                    let _ = self.sign_out();
                    anyhow!(NotSignedIn).context(format!("refresh token rejected: {e}"))
                } else {
                    e.context("refreshing access token")
                }
            })?;
        self.store_access(&resp);
        Ok(resp.access_token)
    }

    fn store_access(&self, resp: &TokenResponse) {
        *self.access.lock().unwrap() = Some((
            resp.access_token.clone(),
            Instant::now() + Duration::from_secs(resp.expires_in),
        ));
    }

    /// Starts an authorization-code + PKCE flow for `redirect_uri`. The
    /// front-end opens `request.url` in a browser (desktop: system browser;
    /// iOS: ASWebAuthenticationSession) and passes the returned `code` to
    /// [`Auth::complete_sign_in`].
    pub fn begin_sign_in(&self, redirect_uri: &str) -> Result<AuthRequest> {
        if self.google.client_id.trim().is_empty() {
            bail!("google.client_id is not set in config.toml");
        }
        let verifier = random_token(32)?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_token(16)?;
        let mut url = url::Url::parse(AUTH_URL)?;
        url.query_pairs_mut()
            .append_pair("client_id", self.google.client_id.trim())
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", SCOPE)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &state);
        Ok(AuthRequest {
            url: url.into(),
            state,
            verifier,
            redirect_uri: redirect_uri.to_string(),
        })
    }

    /// Exchanges the authorization code and stores the refresh token.
    pub fn complete_sign_in(&self, request: &AuthRequest, code: &str) -> Result<()> {
        let mut form = vec![
            ("client_id", self.google.client_id.trim()),
            ("code", code),
            ("code_verifier", request.verifier.as_str()),
            ("redirect_uri", request.redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ];
        // iOS OAuth clients have no secret.
        if !self.google.client_secret.trim().is_empty() {
            form.push(("client_secret", self.google.client_secret.trim()));
        }
        let resp: TokenResponse = self
            .http
            .post_form(TOKEN_URL, &form)
            .context("exchanging authorization code")?;
        let refresh = resp
            .refresh_token
            .as_deref()
            .ok_or_else(|| anyhow!("Google did not return a refresh token"))?;
        self.entry()?
            .set_password(refresh)
            .context("saving refresh token to credential store")?;
        *self.refresh.lock().unwrap() = Some(Some(refresh.to_string()));
        self.store_access(&resp);
        log::info!("signed in");
        Ok(())
    }

    /// Starts a loopback sign-in: listens on a random 127.0.0.1 port that
    /// Google redirects to. Show [`LoopbackSignIn::url`] in a browser, then
    /// call [`Auth::finish_loopback_sign_in`] (blocking).
    pub fn start_loopback_sign_in(&self) -> Result<LoopbackSignIn> {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").context("binding loopback port")?;
        let redirect_uri = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let request = self.begin_sign_in(&redirect_uri)?;
        Ok(LoopbackSignIn {
            listener,
            request,
            cancelled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Waits (up to 5 minutes, or until cancelled) for the redirect, then
    /// exchanges the code and stores the refresh token.
    pub fn finish_loopback_sign_in(&self, pending: &LoopbackSignIn) -> Result<()> {
        let code = loopback::wait_for_code(&pending.listener, &pending.request.state, &pending.cancelled)?;
        self.complete_sign_in(&pending.request, &code)
    }

    /// Desktop sign-in: opens the system browser and waits (blocking).
    #[cfg(feature = "loopback-auth")]
    pub fn sign_in(&self) -> Result<()> {
        let pending = self.start_loopback_sign_in()?;
        log::info!("opening browser for Google sign-in");
        open::that_detached(pending.url()).context("opening browser")?;
        self.finish_loopback_sign_in(&pending)
    }
}

/// A loopback sign-in in progress.
pub struct LoopbackSignIn {
    listener: std::net::TcpListener,
    request: AuthRequest,
    cancelled: std::sync::atomic::AtomicBool,
}

impl LoopbackSignIn {
    /// The Google consent page to show.
    pub fn url(&self) -> &str {
        &self.request.url
    }

    /// Makes a pending `finish_loopback_sign_in` return an error.
    pub fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Desktop: keyring's all-in-one API picks the platform store (macOS
/// Keychain, Windows Credential Manager, Secret Service).
#[cfg(not(target_os = "ios"))]
type CredentialEntry = keyring::Entry;

#[cfg(not(target_os = "ios"))]
fn open_entry(service: &str, user: &str) -> keyring::Result<CredentialEntry> {
    keyring::Entry::new(service, user)
}

/// iOS: keyring's all-in-one API has no iOS store, so register the
/// data-protection keychain store with keyring-core once and use it directly.
#[cfg(target_os = "ios")]
type CredentialEntry = keyring_core::Entry;

#[cfg(target_os = "ios")]
fn open_entry(service: &str, user: &str) -> keyring::Result<CredentialEntry> {
    static INIT: std::sync::OnceLock<keyring::Result<()>> = std::sync::OnceLock::new();
    let init = INIT.get_or_init(|| {
        let store = apple_native_keyring_store::protected::Store::new()?;
        keyring_core::set_default_store(store);
        Ok(())
    });
    if let Err(e) = init {
        return Err(keyring::Error::PlatformFailure(e.to_string().into()));
    }
    keyring_core::Entry::new(service, user)
}

/// An in-progress sign-in (PKCE verifier and CSRF state).
pub struct AuthRequest {
    pub url: String,
    pub state: String,
    verifier: String,
    redirect_uri: String,
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("getrandom: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(buf))
}

mod loopback {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use anyhow::{Result, anyhow, bail};

    const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

    pub(super) fn wait_for_code(
        listener: &TcpListener,
        expected_state: &str,
        cancelled: &AtomicBool,
    ) -> Result<String> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        loop {
            if cancelled.load(Ordering::Relaxed) {
                bail!("sign-in cancelled");
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    if let Some(result) = handle_redirect(stream, expected_state)? {
                        return result;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() > deadline {
                        bail!("timed out waiting for Google sign-in");
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Returns `None` for unrelated requests (e.g. /favicon.ico).
    fn handle_redirect(
        mut stream: TcpStream,
        expected_state: &str,
    ) -> Result<Option<Result<String>>> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line)?;
        let path = line.split_whitespace().nth(1).unwrap_or("/");
        let parsed = url::Url::parse(&format!("http://localhost{path}"))?;
        let param = |k: &str| {
            parsed
                .query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        let (code, state, error) = (param("code"), param("state"), param("error"));
        if code.is_none() && error.is_none() {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            return Ok(None);
        }
        let result = match (code, error) {
            (_, Some(err)) => Err(anyhow!("Google sign-in failed: {err}")),
            (Some(_), _) if state.as_deref() != Some(expected_state) => {
                Err(anyhow!("sign-in state mismatch"))
            }
            (Some(code), None) => Ok(code),
            (None, None) => unreachable!(),
        };
        let body = if result.is_ok() {
            "Signed in to yt-lite. You can close this tab."
        } else {
            "yt-lite sign-in failed. Return to the app for details."
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        Ok(Some(result))
    }
}
