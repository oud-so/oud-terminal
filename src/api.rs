//! Client for the oud.so HTTP API.
//!
//! The catalogue is public; the account (bookmarks, lists) needs a session,
//! which the terminal holds as a token and sends as a bearer, the way the
//! desktop app does. Every request identifies itself with `x-oud-client` so
//! the server can tell terminal listeners apart from the web and apps.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DEFAULT_BASE_URL: &str = "https://api.oud.so";
pub const CLIENT_ID: &str = concat!("oud-terminal/", env!("CARGO_PKG_VERSION"));
/// Who asks for a device code; the server only hands them to apps it knows.
const DEVICE_CLIENT: &str = "oud-terminal";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Station {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub country_code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub bitrate: Option<u32>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub geo_lat: Option<f64>,
    #[serde(default)]
    pub geo_long: Option<f64>,
}

impl Station {
    /// "US · New York NY" style location line, empty when unknown.
    pub fn place(&self) -> String {
        let parts: Vec<&str> = [self.country_code.as_deref(), self.state.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Genre {
    /// Filters go by id: many genres have no slug.
    pub id: String,
    pub canonical: String,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub count: u32,
}

impl Genre {
    pub fn label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.canonical)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Country {
    pub slug: String,
    pub name: String,
    pub iso: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeaturedList {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub item_count: u32,
}

/// One playable address for a station, as ranked by the server.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamOption {
    pub radio_id: String,
    pub url: String,
    pub codec: Option<String>,
    pub bitrate: Option<u32>,
    #[serde(default)]
    pub hls: bool,
}

/// The signed-in account.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// One of the account's lists, without its stations.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserList {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_smart: bool,
    #[serde(default)]
    pub item_count: u32,
}

/// A sign-in started here and finished in a browser.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Where a browser sign-in stands, each time it is asked.
pub enum DevicePoll {
    Waiting,
    /// Asked too often: wait longer between asks.
    SlowDown,
    Approved(String),
    Denied,
    Expired,
}

#[derive(Deserialize)]
struct RadioItem {
    radio: Station,
}

#[derive(Deserialize)]
struct SessionResponse {
    user: Option<User>,
}

#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
}

#[derive(Deserialize)]
struct StreamsResponse {
    streams: Vec<StreamOption>,
}

#[derive(Deserialize)]
struct SearchResponse {
    radios: Vec<Station>,
}

#[derive(Debug, Deserialize)]
pub struct TerminalVersion {
    pub latest: Option<String>,
    pub minimum: Option<String>,
}

#[derive(Clone)]
pub struct Api {
    base: String,
    http: Client,
    /// The session token, shared by every clone so workers see sign-ins.
    token: Arc<RwLock<Option<String>>>,
}

impl Api {
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(CLIENT_ID)
            .timeout(Duration::from_secs(15))
            .build()
            .context("building http client")?;
        let token = Arc::new(RwLock::new(None));
        Ok(Self { base: base.into().trim_end_matches('/').to_string(), http, token })
    }

    pub fn set_token(&self, token: Option<String>) {
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = token;
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req =
            self.http.request(method, format!("{}{}", self.base, path)).header("x-oud-client", CLIENT_ID);
        match self.token.read().unwrap_or_else(|e| e.into_inner()).as_deref() {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }

    fn send(&self, req: RequestBuilder) -> Result<Response> {
        req.send().with_context(|| format!("could not reach {}", self.base))
    }

    /// Fail with the server's own words when it gives any.
    fn check(path: &str, res: Response) -> Result<Response> {
        let status = res.status();
        if status.is_success() {
            return Ok(res);
        }
        let body: Value = res.json().unwrap_or(Value::Null);
        let said = ["message", "error_description", "error"]
            .iter()
            .find_map(|k| body.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()));
        match said {
            Some(message) => bail!("{}", message.trim_end_matches('.').to_lowercase()),
            None => bail!("{path} answered {status}"),
        }
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let res = Self::check(path, self.send(self.request(Method::GET, path))?)?;
        res.json().with_context(|| format!("unexpected response from {path}"))
    }

    fn post(&self, path: &str, body: &Value) -> Result<Response> {
        Self::check(path, self.send(self.request(Method::POST, path).json(body))?)
    }

    /// The newest terminal release, and the oldest the API still works with.
    pub fn terminal_version(&self) -> Result<TerminalVersion> {
        self.get("/v1/terminal/version")
    }

    pub fn search(&self, query: &str) -> Result<Vec<Station>> {
        let q = urlencoding::encode(query);
        Ok(self.get::<SearchResponse>(&format!("/search?q={q}&limit=50"))?.radios)
    }

    pub fn featured(&self) -> Result<Vec<FeaturedList>> {
        Ok(self.get::<Page<FeaturedList>>("/featured/lists")?.items)
    }

    pub fn featured_list(&self, slug: &str) -> Result<Vec<Station>> {
        let slug = urlencoding::encode(slug);
        let page = self.get::<Page<RadioItem>>(&format!("/featured/lists/{slug}"))?;
        Ok(page.items.into_iter().map(|i| i.radio).collect())
    }

    /// Good stations few people here have heard, freshly shuffled each call.
    pub fn discover(&self, count: u32) -> Result<Vec<Station>> {
        Ok(self.get::<Page<Station>>(&format!("/radios/discover?limit={count}"))?.items)
    }

    pub fn stations_tagged(&self, tag: &str, limit: u32) -> Result<Vec<Station>> {
        let tag = urlencoding::encode(tag);
        Ok(self.get::<Page<Station>>(&format!("/radios?tag={tag}&limit={limit}&sort=score"))?.items)
    }

    pub fn genres(&self) -> Result<Vec<Genre>> {
        Ok(self.get::<Page<Genre>>("/genres?limit=300")?.items)
    }

    pub fn countries(&self) -> Result<Vec<Country>> {
        Ok(self.get::<Page<Country>>("/countries?limit=300&sort=stations")?.items)
    }

    /// Stations matching any mix of free text, genre id and country slug, best first.
    pub fn stations(
        &self,
        query: &str,
        genre_id: Option<&str>,
        country: Option<&str>,
    ) -> Result<Vec<Station>> {
        let mut path = String::from("/radios?sort=score&limit=100");
        for (key, value) in [("q", Some(query.trim())), ("genreId", genre_id), ("countrySlug", country)] {
            if let Some(v) = value.filter(|v| !v.is_empty()) {
                path.push_str(&format!("&{key}={}", urlencoding::encode(v)));
            }
        }
        Ok(self.get::<Page<Station>>(&path)?.items)
    }

    /// Playable streams for a station, best first.
    pub fn streams(&self, station_id: &str) -> Result<Vec<StreamOption>> {
        let id = urlencoding::encode(station_id);
        Ok(self.get::<StreamsResponse>(&format!("/radios/{id}/streams"))?.streams)
    }

    /// Tell the server a stream would not play so it can re-check it.
    /// Best effort: failures here are never surfaced to the listener.
    pub fn report_failure(&self, station_id: &str, stream_id: &str, reason: &str) {
        let url = format!("{}/radios/{}/play-failure", self.base, urlencoding::encode(station_id));
        let body = serde_json::json!({ "streamId": stream_id, "reason": reason });
        let _ = self.http.post(url).header("x-oud-client", CLIENT_ID).json(&body).send();
    }

    // ---- signing in ------------------------------------------------------

    /// Start a sign-in to be approved in a browser.
    pub fn device_code(&self) -> Result<DeviceCode> {
        let res = self.post("/auth/device/code", &json!({ "client_id": DEVICE_CLIENT }))?;
        res.json().context("unexpected response from /auth/device/code")
    }

    /// Ask once whether the browser sign-in has been approved.
    pub fn device_poll(&self, device_code: &str) -> Result<DevicePoll> {
        let body = json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": device_code,
            "client_id": DEVICE_CLIENT,
        });
        let res = self.send(self.request(Method::POST, "/auth/device/token").json(&body))?;
        let ok = res.status().is_success();
        let body: Value = res.json().context("unexpected response from /auth/device/token")?;
        if ok {
            let token = body.get("access_token").and_then(Value::as_str);
            return token
                .map(|t| DevicePoll::Approved(t.into()))
                .ok_or_else(|| anyhow!("no session came back"));
        }
        match body.get("error").and_then(Value::as_str) {
            Some("authorization_pending") => Ok(DevicePoll::Waiting),
            Some("slow_down") => Ok(DevicePoll::SlowDown),
            Some("access_denied") => Ok(DevicePoll::Denied),
            Some("expired_token") => Ok(DevicePoll::Expired),
            other => bail!("{}", other.unwrap_or("sign-in failed")),
        }
    }

    /// Email a sign-in code. Signing in with it creates the account if needed.
    pub fn send_code(&self, email: &str) -> Result<()> {
        self.post("/auth/email-otp/send-verification-otp", &json!({ "email": email, "type": "sign-in" }))?;
        Ok(())
    }

    /// Trade an emailed code for a session token.
    pub fn verify_code(&self, email: &str, code: &str) -> Result<String> {
        let res = self.post("/auth/sign-in/email-otp", &json!({ "email": email, "otp": code }))?;
        // The bearer plugin hands the token out in a header; the body has it too.
        let header = res.headers().get("set-auth-token").and_then(|v| v.to_str().ok()).map(String::from);
        let body: Value = res.json().unwrap_or(Value::Null);
        header
            .or_else(|| body.get("token").and_then(Value::as_str).map(String::from))
            .ok_or_else(|| anyhow!("no session came back"))
    }

    /// Who the current token belongs to; `None` when it is missing or no longer valid.
    pub fn session(&self) -> Result<Option<User>> {
        Ok(self.get::<SessionResponse>("/me/session")?.user)
    }

    /// End a session on the server too. Takes the token itself, since the
    /// one held here is dropped the moment the person signs out. Best effort.
    pub fn sign_out(&self, token: &str) {
        let url = format!("{}/auth/sign-out", self.base);
        let req = self.http.post(url).header("x-oud-client", CLIENT_ID).bearer_auth(token).json(&json!({}));
        let _ = req.send();
    }

    // ---- the account's library --------------------------------------------

    pub fn bookmarks(&self) -> Result<Vec<Station>> {
        Ok(self.get::<Page<RadioItem>>("/me/bookmarks")?.items.into_iter().map(|i| i.radio).collect())
    }

    pub fn set_bookmark(&self, station_id: &str, on: bool) -> Result<()> {
        if on {
            self.post("/me/bookmarks", &json!({ "radioId": station_id }))?;
        } else {
            let path = format!("/me/bookmarks/{}", urlencoding::encode(station_id));
            Self::check(&path, self.send(self.request(Method::DELETE, &path))?)?;
        }
        Ok(())
    }

    pub fn lists(&self) -> Result<Vec<UserList>> {
        Ok(self.get::<Page<UserList>>("/me/lists")?.items)
    }

    /// A list's stations; smart lists are resolved by the server.
    pub fn list_stations(&self, list_id: &str) -> Result<Vec<Station>> {
        let path = format!("/me/lists/by-id/{}", urlencoding::encode(list_id));
        Ok(self.get::<Page<RadioItem>>(&path)?.items.into_iter().map(|i| i.radio).collect())
    }

    /// A plain client for opening audio streams: no overall timeout, since a
    /// radio stream is meant to stay open for hours.
    pub fn stream_client() -> Result<Client> {
        Client::builder()
            .user_agent(CLIENT_ID)
            .connect_timeout(Duration::from_secs(8))
            .build()
            .context("building stream client")
    }
}
