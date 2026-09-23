//! Forced RSS feed fetch/parse (M11 WS1d / Workstream B4a).
//!
//! Python `rss/service.py::fetch_feed` downloads with `httpx.get(follow_redirects=True)`
//! and parses via `feedparser`. This module ports that behavior behind the Rust
//! capability/SSRF policy: `NetworkPolicy`-pinned egress with a per-hop redirect
//! re-check (deliberate hardening vs Python's unchecked redirects), a 20 s
//! timeout, a 5 MB body cap, and a bounded RSS/Atom field mapping. Fetched
//! content is untrusted: `quick-xml` never expands DTD/external entities, field
//! text is length-capped during capture, and only the five predefined XML
//! entities (plus numeric character references) are decoded.
//!
//! Divergences from Python, recorded deliberately:
//! - Every redirect hop is re-pinned and re-checked (Python follows blindly),
//!   and a hop may not pivot into loopback unless the original subscription
//!   URL is that same loopback host.
//! - Redirects are capped at 5 (Python/httpx allows 20).
//! - The request sends `User-Agent: CoolAIHarness/0.1` (Python sends the httpx default).
//! - The 5 MB cap aborts mid-download (Python buffers first).
//! - Feed dates accept RFC 822 and RFC 3339 only (`feedparser` is broader); an
//!   unparsable date is stored as `NULL` rather than failing the entry.
//! - `published`/`pubDate` win over `updated`/`date` by tag, matching Python's
//!   `published_parsed`-then-`updated_parsed` preference.
//! - The parser covers RSS 2.0, Atom 1.0 and RDF/RSS 1.0 `item` elements for the
//!   fields the store keeps (guid/title/link/author/summary/published); other
//!   elements are ignored (`entry.content` is not stored, matching Python).
//! - `content_hash`/guid fallback hash whitespace-normalized, entity-decoded
//!   text (Python hashes feedparser's normalized values); irregular whitespace
//!   can therefore diverge across runtimes.
//! - `last_error` is masked with `mask_secrets` before it is stored (network
//!   errors can embed request URLs with query credentials); the client-bound
//!   `safe_details` path is re-masked as well.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cool_app_server::{RssFeedFetch, RssFetchError};
use cool_protocol::RssFetchResult;
use cool_security::{NetworkPolicy, mask_secrets};
use cool_store::LegacyStore;
use cool_store::StoreError;
use cool_store::domains::rss::NewRssEntry;
use cool_store::time::{days_from_civil, python_datetime};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use sha2::{Digest, Sha256};

const FETCH_TIMEOUT_SECS: u64 = 20;
const MAX_FEED_BYTES: usize = 5_000_000;
const MAX_ENTRIES_PER_SUB: usize = 500;
const MAX_PARSED_ENTRIES: usize = 5_000;
const MAX_REDIRECTS: u8 = 5;
const TITLE_CHARS: usize = 500;
const SUMMARY_CHARS: usize = 2_000;
const LAST_ERROR_CHARS: usize = 500;
const CAPTURE_CHARS: usize = 65_536;
const USER_AGENT: &str = "CoolAIHarness/0.1";

/// CLI forced feed fetch over the legacy RSS store + pinned egress.
pub struct CliRssFeedFetch {
    store: Option<Arc<LegacyStore>>,
}

impl CliRssFeedFetch {
    pub fn new(store: Option<Arc<LegacyStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl RssFeedFetch for CliRssFeedFetch {
    async fn fetch_now(
        &self,
        actor: &str,
        subscription_id: i64,
        idempotency_key: &str,
    ) -> Result<RssFetchResult, RssFetchError> {
        let Some(store) = self.store.clone() else {
            return Err(RssFetchError::Unavailable(
                "rss store unavailable".to_owned(),
            ));
        };
        let actor_id = actor.to_owned();
        // Fingerprint covers only the target id (the sole varying input). Not
        // `legacy::fingerprint(&params)`: a later params-shape change must not
        // spuriously conflict keys already stored.
        let fingerprint = format!(r#"{{"id":{subscription_id}}}"#);
        let store_for_action = store.clone();
        let actor_for_action = actor_id.clone();
        store
            .run_idempotent_async(
                &actor_id,
                "rss.fetch_now",
                idempotency_key,
                &fingerprint,
                move || {
                    let store = store_for_action.clone();
                    let actor = actor_for_action.clone();
                    async move { fetch_and_apply(&store, &actor, subscription_id).await }
                },
            )
            .await
            .map(|idempotent| idempotent.value)
            .map_err(map_store_error)
    }
}

fn map_store_error(error: StoreError) -> RssFetchError {
    match error {
        StoreError::NotFound(_) => RssFetchError::NotFound,
        StoreError::Conflict(message) => RssFetchError::Conflict(message),
        other => RssFetchError::Failed(other.to_string()),
    }
}

async fn fetch_and_apply(
    store: &LegacyStore,
    actor: &str,
    subscription_id: i64,
) -> Result<RssFetchResult, StoreError> {
    let subscription = store.get_subscription(actor, subscription_id)?;
    let new_entries = match download_feed(&subscription.url).await {
        Ok(body) => match parse_feed(&body) {
            Ok(feed) => apply_feed(store, actor, subscription_id, feed)?,
            Err(error) => record_failure(store, actor, subscription_id, &error)?,
        },
        Err(error) => record_failure(store, actor, subscription_id, &error)?,
    };
    Ok(RssFetchResult {
        subscription_id,
        new_entries,
    })
}

/// Secret-mask then bound the error text that will be stored in `last_error`.
fn masked_error_message(error: &str) -> String {
    let masked = mask_secrets(error);
    masked.chars().take(LAST_ERROR_CHARS).collect()
}

/// Fetch/parse failure: stamp `last_fetched_at`/`last_error` and answer 0 new
/// entries (Python `fetch_feed` never raises to the caller and never touches
/// `entry_count` on the error path). The error text is secret-masked before it
/// is stored: reqwest/URL errors can embed the request URL.
fn record_failure(
    store: &LegacyStore,
    actor: &str,
    subscription_id: i64,
    error: &str,
) -> Result<i64, StoreError> {
    let message = masked_error_message(error);
    store.record_fetch_result(
        actor,
        subscription_id,
        &cool_store::time::now_python(),
        Some(&message),
    )?;
    Ok(0)
}

struct ParsedFeed {
    title: Option<String>,
    site_url: Option<String>,
    entries: Vec<ParsedEntry>,
}

struct ParsedEntry {
    guid: String,
    title: Option<String>,
    link: Option<String>,
    author: Option<String>,
    summary: Option<String>,
    published_at: Option<String>,
    content_hash: String,
}

fn apply_feed(
    store: &LegacyStore,
    actor: &str,
    subscription_id: i64,
    feed: ParsedFeed,
) -> Result<i64, StoreError> {
    store.fill_subscription_meta(
        actor,
        subscription_id,
        feed.title.as_deref(),
        feed.site_url.as_deref(),
    )?;
    let mut new_entries = 0i64;
    for entry in &feed.entries {
        let inserted = store.insert_entry_if_new(
            actor,
            subscription_id,
            &NewRssEntry {
                guid: entry.guid.clone(),
                title: entry.title.clone(),
                link: entry.link.clone(),
                author: entry.author.clone(),
                summary: entry.summary.clone(),
                published_at: entry.published_at.clone(),
                content_hash: Some(entry.content_hash.clone()),
            },
        )?;
        if inserted.is_some() {
            new_entries += 1;
        }
    }
    if new_entries > 0 {
        store.prune_subscription_entries(actor, subscription_id, MAX_ENTRIES_PER_SUB)?;
    }
    // Success recounts `entry_count` to `COUNT(*)` inside the store (self-healing).
    store.record_fetch_result(
        actor,
        subscription_id,
        &cool_store::time::now_python(),
        None,
    )?;
    Ok(new_entries)
}

/// True when a redirect hop is allowed: loopback is reachable only when the
/// *original* subscription URL is that same loopback host.
fn redirect_hop_allowed(original_host: &str, original_loopback: bool, next: &url::Url) -> bool {
    if is_loopback_url(next) {
        original_loopback && policy_host(next) == original_host
    } else {
        true
    }
}

/// Download a feed body with pinned egress and a per-hop redirect re-check.
///
/// Loopback is only reachable when the *original* subscription URL is that same
/// loopback host: a public feed must not be able to redirect into `127.0.0.1`.
pub async fn download_feed(url: &str) -> Result<Vec<u8>, String> {
    let original = url::Url::parse(url).map_err(|error| format!("invalid feed URL: {error}"))?;
    let original_host = policy_host(&original);
    let original_loopback = is_loopback_host(&original_host);
    let mut current = original;
    let mut redirects = 0u8;
    loop {
        let allow_loopback = original_loopback && policy_host(&current) == original_host;
        match fetch_once(&current, allow_loopback).await? {
            FetchOutcome::Body(body) => return Ok(body),
            FetchOutcome::Redirect(location) => {
                redirects += 1;
                if redirects > MAX_REDIRECTS {
                    return Err("feed exceeded the redirect limit".to_owned());
                }
                let next = current
                    .join(&location)
                    .map_err(|error| format!("invalid feed redirect: {error}"))?;
                if !redirect_hop_allowed(&original_host, original_loopback, &next) {
                    return Err("feed redirect to loopback denied".to_owned());
                }
                current = next;
            }
        }
    }
}

/// Unbracketed host for `NetworkPolicy` seeding (IPv6 literals come back
/// bracketed from `Url::host_str`).
fn policy_host(url: &url::Url) -> String {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.to_ascii_lowercase(),
        Some(url::Host::Ipv4(address)) => address.to_string(),
        Some(url::Host::Ipv6(address)) => address.to_string(),
        None => String::new(),
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn is_loopback_url(url: &url::Url) -> bool {
    url.host().is_some_and(|host| match host {
        url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    })
}

enum FetchOutcome {
    Body(Vec<u8>),
    Redirect(String),
}

async fn fetch_once(url: &url::Url, allow_loopback: bool) -> Result<FetchOutcome, String> {
    let host = policy_host(url);
    if host.is_empty() {
        return Err("feed URL has no host".to_owned());
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "feed URL has no port".to_owned())?;
    let resolved = tokio::time::timeout(
        Duration::from_secs(FETCH_TIMEOUT_SECS),
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    .map_err(|_| "feed host lookup timed out".to_owned())?
    .map_err(|error| format!("feed host lookup failed: {error}"))?
    .collect::<Vec<_>>();
    let loopback = allow_loopback && is_loopback_host(&host);
    let policy = if loopback {
        NetworkPolicy::new([host.clone()]).loopback_only()
    } else {
        NetworkPolicy::new([host.clone()])
    };
    let pinned = policy
        .pin(url.as_str(), resolved.iter().map(|socket| socket.ip()))
        .map_err(|error| format!("feed URL denied: {error}"))?;
    let address = resolved
        .iter()
        .find(|candidate| pinned.addresses.contains(&candidate.ip()))
        .copied()
        .ok_or_else(|| "no pinned feed address".to_owned())?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .no_proxy()
        .resolve(&host, address)
        .build()
        .map_err(|error| format!("feed client failed: {error}"))?;
    let response = client
        .get(url.clone())
        .header("user-agent", USER_AGENT)
        .send()
        .await
        .map_err(|error| format!("feed request failed: {error}"))?;
    let status = response.status();
    if status.is_redirection() {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| "feed redirect had no Location".to_owned())?
            .to_owned();
        return Ok(FetchOutcome::Redirect(location));
    }
    if !status.is_success() {
        return Err(format!("feed returned HTTP {}", status.as_u16()));
    }
    let mut response = response;
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("feed body failed: {error}"))?
    {
        if body.len() + chunk.len() > MAX_FEED_BYTES {
            return Err(format!("Feed body exceeds {MAX_FEED_BYTES} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(FetchOutcome::Body(body))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Capture {
    FeedTitle,
    FeedLink,
    ItemTitle,
    ItemLink,
    ItemSummary,
    ItemAuthor,
    ItemGuid,
    ItemPublished,
    ItemUpdated,
}

#[derive(Default)]
struct ItemDraft {
    guid: Option<String>,
    title: Option<String>,
    link: Option<String>,
    author: Option<String>,
    summary: Option<String>,
    /// `pubDate`/`published` (Python `published_parsed`, preferred).
    published_at: Option<String>,
    /// `updated`/`date` (Python `updated_parsed`, fallback only).
    updated_at: Option<String>,
}

/// Parse an RSS 2.0 / Atom 1.0 / RDF feed body into the stored entry fields.
fn parse_feed(body: &[u8]) -> Result<ParsedFeed, String> {
    let xml = std::str::from_utf8(body).map_err(|_| "feed body is not valid UTF-8".to_owned())?;
    let mut reader = Reader::from_str(xml);
    // No `trim_text`: whitespace around entity references would be lost per
    // fragment. `normalize_text` collapses runs when a capture closes.

    let mut feed_title: Option<String> = None;
    let mut feed_link: Option<String> = None;
    let mut entries: Vec<ParsedEntry> = Vec::new();
    let mut item: Option<ItemDraft> = None;
    let mut capture: Option<Capture> = None;
    let mut capture_tag: String = String::new();
    let mut text = String::new();
    let mut captured = 0usize;
    let mut stack: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if capture.is_some() {
                    // Nested markup inside a captured field (for example HTML in
                    // `<description>` or `<author><name>`): keep accumulating.
                    stack.push(name);
                    continue;
                }
                match name.as_str() {
                    "item" | "entry" => {
                        item = Some(ItemDraft::default());
                    }
                    "link" => {
                        let href = attribute(&event, "href");
                        let rel = attribute(&event, "rel");
                        let alternate = rel.as_deref().unwrap_or("alternate") == "alternate";
                        if let Some(href) = href.filter(|_| alternate) {
                            if let Some(draft) = item.as_mut() {
                                if draft.link.is_none() {
                                    draft.link = Some(href);
                                }
                            } else if feed_link.is_none() {
                                feed_link = Some(href);
                            }
                        }
                        // RSS 2.0 `<link>` has the URL as text content.
                        if attribute(&event, "href").is_none() {
                            start_capture(
                                &mut capture,
                                &mut capture_tag,
                                &mut text,
                                &mut captured,
                                name.clone(),
                                if item.is_some() {
                                    Capture::ItemLink
                                } else {
                                    Capture::FeedLink
                                },
                            );
                        }
                    }
                    "author" | "creator" | "managingEditor" if item.is_some() => {
                        start_capture(
                            &mut capture,
                            &mut capture_tag,
                            &mut text,
                            &mut captured,
                            name.clone(),
                            Capture::ItemAuthor,
                        );
                    }
                    "title" => {
                        start_capture(
                            &mut capture,
                            &mut capture_tag,
                            &mut text,
                            &mut captured,
                            name.clone(),
                            if item.is_some() {
                                Capture::ItemTitle
                            } else {
                                Capture::FeedTitle
                            },
                        );
                    }
                    "description" | "summary" if item.is_some() => {
                        start_capture(
                            &mut capture,
                            &mut capture_tag,
                            &mut text,
                            &mut captured,
                            name.clone(),
                            Capture::ItemSummary,
                        );
                    }
                    "guid" | "id" if item.is_some() => {
                        start_capture(
                            &mut capture,
                            &mut capture_tag,
                            &mut text,
                            &mut captured,
                            name.clone(),
                            Capture::ItemGuid,
                        );
                    }
                    "pubDate" | "published" if item.is_some() => {
                        start_capture(
                            &mut capture,
                            &mut capture_tag,
                            &mut text,
                            &mut captured,
                            name.clone(),
                            Capture::ItemPublished,
                        );
                    }
                    "updated" | "date" if item.is_some() => {
                        start_capture(
                            &mut capture,
                            &mut capture_tag,
                            &mut text,
                            &mut captured,
                            name.clone(),
                            Capture::ItemUpdated,
                        );
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "link" {
                    let href = attribute(&event, "href");
                    let rel = attribute(&event, "rel");
                    if rel.as_deref().unwrap_or("alternate") == "alternate"
                        && let Some(href) = href
                    {
                        if let Some(draft) = item.as_mut() {
                            if draft.link.is_none() {
                                draft.link = Some(href);
                            }
                        } else if feed_link.is_none() {
                            feed_link = Some(href);
                        }
                    }
                }
            }
            Ok(Event::Text(event)) => {
                if capture.is_some() {
                    let unescaped = quick_xml::escape::unescape(event.as_ref())
                        .map_err(|error| format!("feed text unescape failed: {error}"))?;
                    push_capped(&mut text, &mut captured, &unescaped);
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if capture.is_some() {
                    // Only the five predefined entities and numeric character
                    // references resolve; a custom entity (DOCTYPE) is never
                    // expanded.
                    let reference = event.as_ref();
                    push_capped(&mut text, &mut captured, &resolve_reference(reference));
                }
            }
            Ok(Event::CData(event)) => {
                if capture.is_some() {
                    push_capped(&mut text, &mut captured, event.as_ref());
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                let _finished = stack.pop();
                if capture.is_some() && name == capture_tag {
                    let value = normalize_text(&text);
                    match capture.expect("capture") {
                        Capture::FeedTitle => {
                            feed_title = Some(value).filter(|value| !value.is_empty());
                        }
                        Capture::FeedLink => {
                            if feed_link.is_none() {
                                feed_link = Some(value).filter(|value| !value.is_empty());
                            }
                        }
                        Capture::ItemTitle => {
                            if let Some(draft) = item.as_mut() {
                                draft.title = Some(value).filter(|value| !value.is_empty());
                            }
                        }
                        Capture::ItemLink => {
                            if let Some(draft) = item.as_mut()
                                && draft.link.is_none()
                            {
                                draft.link = Some(value).filter(|value| !value.is_empty());
                            }
                        }
                        Capture::ItemSummary => {
                            if let Some(draft) = item.as_mut() {
                                draft.summary = Some(value).filter(|value| !value.is_empty());
                            }
                        }
                        Capture::ItemAuthor => {
                            if let Some(draft) = item.as_mut()
                                && draft.author.is_none()
                            {
                                draft.author = Some(value).filter(|value| !value.is_empty());
                            }
                        }
                        Capture::ItemGuid => {
                            if let Some(draft) = item.as_mut() {
                                draft.guid = Some(value).filter(|value| !value.is_empty());
                            }
                        }
                        Capture::ItemPublished => {
                            if let Some(draft) = item.as_mut()
                                && draft.published_at.is_none()
                            {
                                draft.published_at = parse_feed_date(&value);
                            }
                        }
                        Capture::ItemUpdated => {
                            if let Some(draft) = item.as_mut()
                                && draft.updated_at.is_none()
                            {
                                draft.updated_at = parse_feed_date(&value);
                            }
                        }
                    }
                    capture = None;
                    capture_tag.clear();
                    text.clear();
                }
                if (name == "item" || name == "entry")
                    && let Some(draft) = item.take()
                    && entries.len() < MAX_PARSED_ENTRIES
                {
                    entries.push(finalize_entry(draft));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(format!("Feed parse error: {error}")),
        }
    }
    if entries.is_empty() && feed_title.is_none() && feed_link.is_none() {
        return Err("Feed parse error: no recognizable RSS/Atom content".to_owned());
    }
    Ok(ParsedFeed {
        title: feed_title.map(|value| truncate(value, TITLE_CHARS)),
        site_url: feed_link.map(|value| truncate(value, TITLE_CHARS)),
        entries,
    })
}

fn finalize_entry(draft: ItemDraft) -> ParsedEntry {
    // Python hashes and GUID-falls-back over the *untruncated* fields, then
    // truncates title/summary for storage.
    let raw_title = draft.title.unwrap_or_default();
    let raw_link = draft.link.unwrap_or_default();
    let raw_summary = draft.summary.unwrap_or_default();
    let content_hash = hex_sha256(format!("{raw_title}|{raw_link}|{raw_summary}").as_bytes());
    let guid = draft
        .guid
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if raw_link.is_empty() {
                hex_sha256(raw_title.as_bytes())
            } else {
                hex_sha256(raw_link.as_bytes())
            }
        });
    let title = truncate(raw_title, TITLE_CHARS);
    let title = (!title.is_empty()).then_some(title);
    let link = (!raw_link.is_empty()).then_some(raw_link);
    let summary = truncate(raw_summary, SUMMARY_CHARS);
    let summary = (!summary.is_empty()).then_some(summary);
    ParsedEntry {
        guid,
        title,
        link,
        author: draft.author.filter(|value| !value.is_empty()),
        summary,
        // Python: `published_parsed` then `updated_parsed`.
        published_at: draft.published_at.or(draft.updated_at),
        content_hash,
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

fn truncate(value: String, max: usize) -> String {
    value.chars().take(max).collect()
}

fn normalize_text(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_capped(buffer: &mut String, taken: &mut usize, extra: &str) {
    if *taken >= CAPTURE_CHARS {
        return;
    }
    let remaining = CAPTURE_CHARS - *taken;
    let clipped: String = extra.chars().take(remaining).collect();
    *taken += clipped.chars().count();
    buffer.push_str(&clipped);
}

/// Resolve `&ref;` / `&#123;` with no DTD expansion: the five predefined
/// entities and numeric character references only.
fn resolve_reference(reference: &str) -> String {
    match reference {
        "amp" => "&".to_owned(),
        "lt" => "<".to_owned(),
        "gt" => ">".to_owned(),
        "quot" => "\"".to_owned(),
        "apos" => "'".to_owned(),
        _ => {
            if let Some(hex) = reference
                .strip_prefix("#x")
                .or_else(|| reference.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_default()
            } else if let Some(dec) = reference.strip_prefix('#') {
                dec.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
    }
}

fn local_name(name: &str) -> String {
    match name.rsplit_once(':') {
        Some((_, local)) => local.to_owned(),
        None => name.to_owned(),
    }
}

fn attribute(event: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    for attribute in event.attributes().flatten() {
        let name = local_name(attribute.key.as_ref());
        if name == key {
            let value = quick_xml::escape::unescape(&attribute.value).unwrap_or_default();
            return Some(value.into_owned());
        }
    }
    None
}

fn start_capture(
    capture: &mut Option<Capture>,
    capture_tag: &mut String,
    text: &mut String,
    captured: &mut usize,
    tag: String,
    kind: Capture,
) {
    *capture = Some(kind);
    *capture_tag = tag;
    text.clear();
    *captured = 0;
}

/// Parse the date formats the port accepts (RFC 822 and RFC 3339) into the
/// SQLAlchemy/SQLite representation. Microseconds are dropped (Python takes
/// the first six `struct_time` fields).
fn parse_feed_date(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(seconds) = parse_rfc3339(value) {
        return Some(python_datetime(seconds, 0));
    }
    if let Some(seconds) = parse_rfc822(value) {
        return Some(python_datetime(seconds, 0));
    }
    None
}

fn parse_rfc3339(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let year: i64 = value.get(0..4)?.parse().ok()?;
    if bytes[4] != b'-' {
        return None;
    }
    let month: u32 = value.get(5..7)?.parse().ok()?;
    if bytes[7] != b'-' {
        return None;
    }
    let day: u32 = value.get(8..10)?.parse().ok()?;
    if bytes[10] != b'T' && bytes[10] != b't' && bytes[10] != b' ' {
        return None;
    }
    let hour: i64 = value.get(11..13)?.parse().ok()?;
    if bytes[13] != b':' {
        return None;
    }
    let minute: i64 = value.get(14..16)?.parse().ok()?;
    if bytes[16] != b':' {
        return None;
    }
    let second: i64 = value.get(17..19)?.parse().ok()?;
    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    let zone = value.get(index..)?;
    let offset = parse_offset(zone)?;
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

fn parse_offset(zone: &str) -> Option<i64> {
    let zone = zone.trim();
    if zone.is_empty() || zone == "Z" || zone == "z" {
        return Some(0);
    }
    let bytes = zone.as_bytes();
    let sign = match bytes[0] {
        b'+' => 1i64,
        b'-' => -1i64,
        _ => return None,
    };
    if zone.len() < 5 || bytes[3] != b':' {
        return None;
    }
    let hours: i64 = zone.get(1..3)?.parse().ok()?;
    let minutes: i64 = zone.get(4..6)?.parse().ok()?;
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn parse_rfc822(value: &str) -> Option<i64> {
    let mut parts = value.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 4 {
        return None;
    }
    // Drop the optional weekday prefix ("Mon,").
    if parts[0].ends_with(',') {
        parts.remove(0);
    }
    if parts.len() < 4 {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month = month_number(parts[1])?;
    let year = normalize_two_digit_year(parts[2].parse::<i64>().ok()?)?;
    let time = parts[3];
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let second: i64 = time_parts.next().unwrap_or("0").parse().ok()?;
    let offset = match parts.get(4) {
        None => 0,
        Some(zone) => parse_rfc822_zone(zone)?,
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

fn parse_rfc822_zone(zone: &str) -> Option<i64> {
    match zone {
        "GMT" | "UT" | "UTC" => Some(0),
        "Z" | "z" => Some(0),
        _ => {
            if zone.len() == 5 && (zone.starts_with('+') || zone.starts_with('-')) {
                parse_offset(zone)
            } else {
                // Named military/US zones are treated as an unparsable date so
                // the entry is stored without `published_at` (fail-open on the
                // field, never on the fetch).
                None
            }
        }
    }
}

fn month_number(name: &str) -> Option<u32> {
    match name.get(0..3)?.to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn normalize_two_digit_year(year: i64) -> Option<i64> {
    if (0..100).contains(&year) {
        Some(if year < 70 { 2000 + year } else { 1900 + year })
    } else if (1..=9999).contains(&year) {
        Some(year)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Example &amp; Blog</title>
  <link>https://example.com/</link>
  <item>
    <title>First &lt;post&gt;</title>
    <link>https://example.com/a</link>
    <description><![CDATA[Hello <b>world</b>]]></description>
    <author>ann@example.com</author>
    <guid isPermaLink="false">tag:example,2026:a</guid>
    <pubDate>Mon, 06 Jan 2026 08:49:37 GMT</pubDate>
  </item>
  <item>
    <title>Second</title>
    <link>https://example.com/b</link>
    <description>plain</description>
    <pubDate>2026-01-07T10:00:00Z</pubDate>
  </item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Feed</title>
  <link rel="alternate" href="https://example.org/"/>
  <entry>
    <id>urn:uuid:1</id>
    <title>Entry one</title>
    <link rel="alternate" href="https://example.org/one"/>
    <summary>Summary text</summary>
    <author><name>Ada</name></author>
    <published>2026-02-01T12:00:00+02:00</published>
  </entry>
</feed>"#;

    #[test]
    fn parses_rss_fields_and_decodes_entities() {
        let feed = parse_feed(RSS.as_bytes()).expect("parse");
        assert_eq!(feed.title.as_deref(), Some("Example & Blog"));
        assert_eq!(feed.site_url.as_deref(), Some("https://example.com/"));
        assert_eq!(feed.entries.len(), 2);
        let first = &feed.entries[0];
        assert_eq!(first.guid, "tag:example,2026:a");
        assert_eq!(first.title.as_deref(), Some("First <post>"));
        assert_eq!(first.summary.as_deref(), Some("Hello <b>world</b>"));
        assert_eq!(first.author.as_deref(), Some("ann@example.com"));
        assert_eq!(
            first.published_at.as_deref(),
            Some("2026-01-06 08:49:37.000000")
        );
        let second = &feed.entries[1];
        assert_eq!(
            second.published_at.as_deref(),
            Some("2026-01-07 10:00:00.000000")
        );
    }

    #[test]
    fn parses_atom_fields_and_author_name() {
        let feed = parse_feed(ATOM.as_bytes()).expect("parse");
        assert_eq!(feed.title.as_deref(), Some("Atom Feed"));
        assert_eq!(feed.site_url.as_deref(), Some("https://example.org/"));
        assert_eq!(feed.entries.len(), 1);
        let entry = &feed.entries[0];
        assert_eq!(entry.guid, "urn:uuid:1");
        assert_eq!(entry.author.as_deref(), Some("Ada"));
        assert_eq!(entry.link.as_deref(), Some("https://example.org/one"));
        assert_eq!(
            entry.published_at.as_deref(),
            Some("2026-02-01 10:00:00.000000")
        );
    }

    #[test]
    fn guid_falls_back_to_link_then_title_hash() {
        let xml = r#"<rss><channel><item><title>Only title</title></item>
            <item><link>https://example.com/x</link></item></channel></rss>"#;
        let feed = parse_feed(xml.as_bytes()).expect("parse");
        assert_eq!(feed.entries.len(), 2);
        let by_link = hex_sha256(b"https://example.com/x");
        let by_title = hex_sha256(b"Only title");
        assert_eq!(feed.entries[1].guid, by_link);
        assert_eq!(feed.entries[0].guid, by_title);
    }

    #[test]
    fn content_hash_matches_python_title_link_summary() {
        let feed = parse_feed(RSS.as_bytes()).expect("parse");
        let expected = hex_sha256(b"First <post>|https://example.com/a|Hello <b>world</b>");
        assert_eq!(feed.entries[0].content_hash, expected);
    }

    #[test]
    fn title_and_summary_are_truncated_like_python() {
        let title = "T".repeat(TITLE_CHARS + 20);
        let summary = "S".repeat(SUMMARY_CHARS + 20);
        let xml = format!(
            r#"<rss><channel><item><title>{title}</title><description>{summary}</description>
            <guid>g</guid></item></channel></rss>"#
        );
        let feed = parse_feed(xml.as_bytes()).expect("parse");
        assert_eq!(
            feed.entries[0].title.as_deref().map(str::len),
            Some(TITLE_CHARS)
        );
        assert_eq!(
            feed.entries[0].summary.as_deref().map(str::len),
            Some(SUMMARY_CHARS)
        );
    }

    #[test]
    fn doctype_entities_are_not_expanded() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE rss [<!ENTITY xxe "expanded">]>
<rss><channel><item><title>&xxe;</title><guid>g</guid></item></channel></rss>"#;
        // quick-xml does not expand the custom entity; either the raw reference
        // survives or the text is dropped — it must never become "expanded".
        match parse_feed(xml.as_bytes()) {
            Ok(feed) => {
                let title = feed.entries[0].title.as_deref().unwrap_or("");
                assert!(!title.contains("expanded"), "entity expanded: {title}");
            }
            Err(error) => {
                // Either "Feed parse error: …" or "feed text unescape failed: …";
                // a custom DOCTYPE entity must never expand either way.
                let _ = error;
            }
        }
    }

    #[test]
    fn empty_body_is_a_parse_error() {
        assert!(parse_feed(b"<html><body>nope</body></html>").is_err());
    }

    #[test]
    fn date_parser_accepts_rfc822_and_rfc3339_only() {
        assert_eq!(
            parse_feed_date("Mon, 06 Jan 2026 08:49:37 GMT").as_deref(),
            Some("2026-01-06 08:49:37.000000")
        );
        assert_eq!(
            parse_feed_date("2026-01-07T10:00:00Z").as_deref(),
            Some("2026-01-07 10:00:00.000000")
        );
        assert_eq!(
            parse_feed_date("2026-01-07T10:00:00+02:30").as_deref(),
            Some("2026-01-07 07:30:00.000000")
        );
        assert_eq!(parse_feed_date("yesterday"), None);
        assert_eq!(parse_feed_date("Mon, 06 Jan 2026 08:49:37 EST"), None);
    }

    #[tokio::test]
    async fn download_denies_a_private_non_loopback_host() {
        let error = download_feed("http://10.0.0.1/feed.xml")
            .await
            .expect_err("a private host must be denied");
        assert!(error.contains("denied"), "unexpected error: {error}");
    }

    #[tokio::test]
    async fn download_denies_an_invalid_url() {
        let error = download_feed("not a url")
            .await
            .expect_err("an invalid URL must fail");
        assert!(error.contains("invalid feed URL"), "unexpected: {error}");
    }

    #[test]
    fn loopback_hosts_and_urls_are_classified() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("10.0.0.1"));

        let loopback = url::Url::parse("http://127.0.0.1/feed").unwrap();
        let ipv6 = url::Url::parse("http://[::1]/feed").unwrap();
        let remote = url::Url::parse("http://example.com/feed").unwrap();
        assert!(is_loopback_url(&loopback));
        assert!(is_loopback_url(&ipv6));
        assert!(!is_loopback_url(&remote));
        assert_eq!(policy_host(&ipv6), "::1", "IPv6 seeds unbracketed");
    }

    #[test]
    fn masked_error_message_redacts_query_credentials_and_bounds_length() {
        let raw = "feed request failed: https://example.com/feed?token=super-secret-value&x=1";
        let masked = masked_error_message(raw);
        assert!(
            !masked.contains("super-secret-value"),
            "query token leaked: {masked}"
        );
        assert!(
            masked.contains("feed request failed"),
            "prefix lost: {masked}"
        );

        let long = "e".repeat(LAST_ERROR_CHARS + 50);
        assert_eq!(
            masked_error_message(&long).chars().count(),
            LAST_ERROR_CHARS
        );
    }

    #[test]
    fn redirect_hop_blocks_pivots_into_loopback() {
        let public = url::Url::parse("https://example.com/feed").unwrap();
        let public_host = policy_host(&public);
        let public_loopback = is_loopback_host(&public_host);

        let pivot = url::Url::parse("http://127.0.0.1:8080/admin").unwrap();
        let pivot6 = url::Url::parse("http://[::1]/admin").unwrap();
        let other_public = url::Url::parse("http://example.org/other").unwrap();
        assert!(
            !redirect_hop_allowed(&public_host, public_loopback, &pivot),
            "a public feed must not reach 127.0.0.1"
        );
        assert!(
            !redirect_hop_allowed(&public_host, public_loopback, &pivot6),
            "a public feed must not reach ::1"
        );
        assert!(redirect_hop_allowed(
            &public_host,
            public_loopback,
            &other_public
        ));

        let local = url::Url::parse("http://127.0.0.1:8080/feed").unwrap();
        let local_host = policy_host(&local);
        let local_loopback = is_loopback_host(&local_host);
        let same = url::Url::parse("http://127.0.0.1:8080/other").unwrap();
        let elsewhere = url::Url::parse("http://localhost:9090/other").unwrap();
        assert!(
            redirect_hop_allowed(&local_host, local_loopback, &same),
            "same-host loopback stays allowed for a loopback subscription"
        );
        assert!(
            !redirect_hop_allowed(&local_host, local_loopback, &elsewhere),
            "a different loopback host is still a pivot"
        );
    }

    #[test]
    fn published_dates_win_over_updated() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <entry>
            <id>urn:1</id>
            <updated>2026-03-03T00:00:00Z</updated>
            <published>2026-03-01T00:00:00Z</published>
          </entry>
          <entry>
            <id>urn:2</id>
            <updated>2026-03-04T00:00:00Z</updated>
          </entry>
        </feed>"#;
        let feed = parse_feed(xml.as_bytes()).expect("parse");
        assert_eq!(
            feed.entries[0].published_at.as_deref(),
            Some("2026-03-01 00:00:00.000000"),
            "published wins even when updated comes first"
        );
        assert_eq!(
            feed.entries[1].published_at.as_deref(),
            Some("2026-03-04 00:00:00.000000"),
            "updated is the fallback"
        );
    }
}
