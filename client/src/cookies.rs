//! The cookies of the tailnet sites the tab has visited.
//!
//! The browser's own cookie store cannot be used: it keeps cookies under the
//! address a page is shown at, not the tailnet address it came from, and the
//! requests are made here, not by the browser. So this jar plays the
//! browser's part. The `cookie_store` crate does the matching rules; this
//! module adds what a browser adds on top:
//!
//! - Scripts see and set cookies through `document.cookie`, but cookies
//!   marked `HttpOnly` are hidden from them and cannot be created or
//!   replaced by them.
//! - A site cannot set a cookie for a whole public suffix, such as `ts.net`,
//!   which would be sent to other people's tailnets.
//! - A cookie marked `SameSite` is held back from requests that another site
//!   caused, so a page elsewhere cannot act with the user's login here.
//! - The jar can be saved as text and loaded again, so logins survive a
//!   reload.

use cookie::{Cookie as RawCookie, SameSite};
use cookie_store::{Cookie, CookieDomain, CookieExpiration, CookieStore};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Default)]
pub struct Jar {
    store: CookieStore,
}

/// One saved cookie, in the form the page stores.
#[derive(Serialize, Deserialize)]
struct Saved {
    /// An address the cookie applies to; loading sets the cookie from here.
    url: String,
    /// The cookie as a `Set-Cookie` line, with an absolute expiry date.
    line: String,
    /// No expiry date: the cookie lasts for the browser session.
    session: bool,
    /// The expiry date as Unix seconds; 0 for a session cookie.
    expires: i64,
}

/// Who caused a request, which decides the cookies it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// The site itself, or the user: every cookie is sent.
    SameSite,
    /// Another site sent the user here by loading a page with GET or HEAD.
    /// `SameSite=Strict` cookies stay behind.
    CrossSiteNavigation,
    /// Anything else another site caused, such as a form it posted or a page
    /// it loaded in a frame. Only `SameSite=None` cookies are sent.
    CrossSite,
}

impl Context {
    /// The context of a `method` request to `target` caused by a page at
    /// `initiator` (an origin such as `http://nas`; `None` when nobody can
    /// say who it was). `top_level` says the request loads a page as a tab
    /// would, not inside a frame of another page.
    pub fn of(initiator: Option<&str>, target: &Url, method: &str, top_level: bool) -> Self {
        let same = initiator
            .and_then(|origin| Url::parse(origin).ok())
            .and_then(|origin| site(&origin))
            .is_some_and(|theirs| site(target) == Some(theirs));
        if same {
            Self::SameSite
        } else if top_level && (method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD")) {
            Self::CrossSiteNavigation
        } else {
            Self::CrossSite
        }
    }

    /// Whether a request in this context carries `cookie`. A cookie that
    /// does not say is treated as `Lax`, as Chrome does.
    fn carries(self, cookie: &Cookie<'static>) -> bool {
        match (self, cookie.same_site()) {
            (Self::SameSite, _) | (_, Some(SameSite::None)) => true,
            (_, Some(SameSite::Strict)) => false,
            (context, Some(SameSite::Lax) | None) => context == Self::CrossSiteNavigation,
        }
    }
}

/// The site `url` belongs to: the name one party registered
/// (`tail1234.ts.net` for `nas.tail1234.ts.net`), or the whole host for an
/// address or a name with no dots. The scheme and port do not count.
fn site(url: &Url) -> Option<String> {
    match url.host()? {
        url::Host::Domain(name) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            Some(psl::domain_str(&name).map(str::to_owned).unwrap_or(name))
        }
        address => Some(address.to_string()),
    }
}

/// Whether `domain` is one that many unrelated parties have names under.
fn is_public_suffix(domain: &str) -> bool {
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    psl::suffix(domain.as_bytes())
        .is_some_and(|suffix| suffix.is_known() && suffix.as_bytes() == domain.as_bytes())
}

/// Most specific path first, then by name, so the order is always the same.
fn in_order<'a>(mut cookies: Vec<&'a Cookie<'static>>) -> Vec<&'a Cookie<'static>> {
    cookies.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then_with(|| a.name().cmp(b.name())));
    cookies
}

fn joined<'a>(cookies: impl IntoIterator<Item = &'a Cookie<'static>>) -> String {
    let pairs: Vec<String> =
        cookies.into_iter().map(|c| format!("{}={}", c.name(), c.value())).collect();
    pairs.join("; ")
}

impl Jar {
    /// Store the cookie in `line` as set by `url`, if the rules allow it.
    /// Returns whether the jar changed.
    fn set(&mut self, url: &Url, line: &str, from_script: bool) -> bool {
        let Ok(raw) = RawCookie::parse(line.to_owned()) else { return false };
        if raw.domain().is_some_and(|domain| {
            is_public_suffix(domain) && url.host_str() != Some(domain.trim_start_matches('.'))
        }) {
            return false;
        }
        let Ok(cookie) = Cookie::try_from_raw_cookie(&raw, url) else { return false };
        if from_script {
            if raw.http_only() == Some(true) {
                return false;
            }
            let domain = match &cookie.domain {
                CookieDomain::HostOnly(d) | CookieDomain::Suffix(d) => d.as_str(),
                _ => return false,
            };
            let hidden = self
                .store
                .get_any(domain, &cookie.path, cookie.name())
                .is_some_and(|existing| existing.http_only() == Some(true) && !existing.is_expired());
            if hidden {
                return false;
            }
        }
        self.store.insert(cookie.into_owned(), url).is_ok()
    }

    /// Take in the `Set-Cookie` lines of a response from `url`. Returns
    /// whether the jar changed.
    pub fn store_response<'a>(&mut self, url: &Url, lines: impl IntoIterator<Item = &'a str>) -> bool {
        let mut changed = false;
        for line in lines {
            changed |= self.set(url, line, false);
        }
        changed
    }

    /// The `Cookie` header for a request to `url` that the site itself or
    /// the user made, if there is anything to send.
    pub fn header_for(&self, url: &Url) -> Option<String> {
        self.header_in(url, Context::SameSite)
    }

    /// The `Cookie` header for a request to `url` made in `context`, if
    /// there is anything to send.
    pub fn header_in(&self, url: &Url, context: Context) -> Option<String> {
        let sent = self.store.matches(url).into_iter().filter(|cookie| context.carries(cookie));
        let header = joined(in_order(sent.collect()));
        (!header.is_empty()).then_some(header)
    }

    /// What `document.cookie` reads on a page at `url`.
    pub fn script_cookies(&self, url: &Url) -> String {
        let visible = self.store.matches(url).into_iter().filter(|c| c.http_only() != Some(true));
        joined(in_order(visible.collect()))
    }

    /// What assigning `line` to `document.cookie` does on a page at `url`.
    /// Returns whether the jar changed.
    pub fn set_from_script(&mut self, url: &Url, line: &str) -> bool {
        self.set(url, line, true)
    }

    /// Every cookie that has not expired, as JSON for the page to store:
    /// `[{ url, line, session, expires }]`.
    pub fn export(&self) -> String {
        let saved: Vec<Saved> = self
            .store
            .iter_unexpired()
            .filter_map(|cookie| {
                let (host, host_only) = match &cookie.domain {
                    CookieDomain::HostOnly(host) => (host.as_str(), true),
                    CookieDomain::Suffix(domain) => (domain.as_str(), false),
                    _ => return None,
                };
                let mut raw: RawCookie<'static> = RawCookie::new(cookie.name().to_owned(), cookie.value().to_owned());
                raw.set_path(cookie.path.to_string());
                if !host_only {
                    raw.set_domain(host.to_owned());
                }
                if let Some(secure) = cookie.secure() {
                    raw.set_secure(secure);
                }
                if let Some(http_only) = cookie.http_only() {
                    raw.set_http_only(http_only);
                }
                if let Some(same_site) = cookie.same_site() {
                    raw.set_same_site(same_site);
                }
                let expires = match cookie.expires {
                    CookieExpiration::AtUtc(at) => {
                        raw.set_expires(at);
                        Some(at.unix_timestamp())
                    }
                    CookieExpiration::SessionEnd => None,
                };
                let scheme = if cookie.secure() == Some(true) { "https" } else { "http" };
                Some(Saved {
                    url: format!("{scheme}://{host}{}", &*cookie.path),
                    line: raw.to_string(),
                    session: expires.is_none(),
                    expires: expires.unwrap_or(0),
                })
            })
            .collect();
        serde_json::to_string(&saved).unwrap_or_else(|_| "[]".to_owned())
    }

    /// Load cookies saved by [`Jar::export`]. Anything unreadable or expired
    /// is skipped.
    pub fn import(&mut self, json: &str) {
        let Ok(saved) = serde_json::from_str::<Vec<Saved>>(json) else { return };
        for cookie in saved {
            if let Ok(url) = Url::parse(&cookie.url) {
                self.set(&url, &cookie.line, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn jar_with(at: &str, lines: &[&str]) -> Jar {
        let mut jar = Jar::default();
        jar.store_response(&url(at), lines.iter().copied());
        jar
    }

    #[test]
    fn sends_cookies_back_to_the_site_that_set_them() {
        let jar = jar_with("http://nas.tail1234.ts.net/app/", &["sid=secret; Path=/; HttpOnly", "theme=dark; Path=/app"]);
        assert_eq!(jar.header_for(&url("http://nas.tail1234.ts.net/app/page")).as_deref(), Some("theme=dark; sid=secret"));
        assert_eq!(jar.header_for(&url("http://nas.tail1234.ts.net/other")).as_deref(), Some("sid=secret"));
        assert_eq!(jar.header_for(&url("http://pi.tail1234.ts.net/")), None);
    }

    #[test]
    fn hides_http_only_cookies_from_scripts() {
        let jar = jar_with("http://nas/", &["sid=secret; Path=/; HttpOnly", "csrftoken=abc; Path=/"]);
        assert_eq!(jar.script_cookies(&url("http://nas/")), "csrftoken=abc");
    }

    #[test]
    fn scripts_can_set_and_delete_their_own_cookies() {
        let mut jar = jar_with("http://nas/", &["csrftoken=abc; Path=/"]);
        assert!(jar.set_from_script(&url("http://nas/"), "theme=dark; path=/"));
        assert_eq!(jar.script_cookies(&url("http://nas/")), "csrftoken=abc; theme=dark");
        assert!(jar.set_from_script(&url("http://nas/"), "theme=; path=/; Max-Age=0"));
        assert_eq!(jar.script_cookies(&url("http://nas/")), "csrftoken=abc");
    }

    #[test]
    fn scripts_cannot_create_or_replace_http_only_cookies() {
        let mut jar = jar_with("http://nas/", &["sid=secret; Path=/; HttpOnly"]);
        assert!(!jar.set_from_script(&url("http://nas/"), "evil=1; Path=/; HttpOnly"));
        assert!(!jar.set_from_script(&url("http://nas/"), "sid=stolen; Path=/"));
        assert_eq!(jar.header_for(&url("http://nas/")).as_deref(), Some("sid=secret"));
    }

    #[test]
    fn secure_cookies_stay_on_https() {
        let jar = jar_with("https://nas.tail1234.ts.net/", &["sid=1; Path=/; Secure"]);
        assert_eq!(jar.header_for(&url("http://nas.tail1234.ts.net/")), None);
        assert_eq!(jar.header_for(&url("https://nas.tail1234.ts.net/")).as_deref(), Some("sid=1"));
    }

    #[test]
    fn a_tailnet_can_share_a_cookie_but_not_with_other_tailnets() {
        let mut jar = Jar::default();
        let nas = url("https://nas.tail1234.ts.net/");
        // Across one tailnet's devices: allowed.
        assert!(jar.store_response(&nas, ["shared=1; Path=/; Domain=tail1234.ts.net"]));
        assert_eq!(jar.header_for(&url("https://pi.tail1234.ts.net/")).as_deref(), Some("shared=1"));
        // Across everyone's tailnets, or a whole top-level domain: refused.
        assert!(!jar.store_response(&nas, ["wide=1; Path=/; Domain=ts.net"]));
        assert!(!jar.store_response(&url("https://shop.example.co.uk/"), ["wide=1; Domain=co.uk"]));
        assert_eq!(jar.header_for(&url("https://nas.tail9999.ts.net/")), None);
        // Another site's cookie: refused.
        assert!(!jar.store_response(&nas, ["x=1; Domain=tail9999.ts.net"]));
    }

    #[test]
    fn saves_and_loads() {
        let mut jar = jar_with(
            "https://nas.tail1234.ts.net/dash?q=1",
            &["remember=long; Path=/; HttpOnly; Secure; Max-Age=3600", "sess=tab; Path=/; HttpOnly", "gone=x; Path=/; Max-Age=0"],
        );
        jar.set_from_script(&url("https://nas.tail1234.ts.net/dash"), "theme=dark; Path=/; Max-Age=3600");

        let saved = jar.export();
        for expected in ["remember=long", "sess=tab", "theme=dark", r#""session":true"#, "HttpOnly"] {
            assert!(saved.contains(expected), "{expected} missing from {saved}");
        }
        for unexpected in ["gone=", "Max-Age", "q=1"] {
            assert!(!saved.contains(unexpected), "{unexpected} present in {saved}");
        }

        let mut loaded = Jar::default();
        loaded.import(&saved);
        let page = url("https://nas.tail1234.ts.net/");
        assert_eq!(loaded.header_for(&page).as_deref(), Some("remember=long; sess=tab; theme=dark"));
        assert_eq!(loaded.script_cookies(&page), "theme=dark", "HttpOnly must stay hidden");
        assert_eq!(loaded.header_for(&url("http://nas.tail1234.ts.net/")).as_deref(), Some("sess=tab; theme=dark"), "Secure must stay on https");

        loaded.import("not json");
        loaded.import(r#"[{"url":"nonsense","line":"a=b","session":true,"expires":0}]"#);
        assert_eq!(loaded.script_cookies(&page), "theme=dark");
    }

    #[test]
    fn works_out_who_counts_as_the_same_site() {
        let nas = url("http://nas.tail1234.ts.net/");
        let of = |from, method, top| Context::of(from, &nas, method, top);
        assert_eq!(of(Some("http://nas.tail1234.ts.net"), "POST", false), Context::SameSite);
        assert_eq!(of(Some("https://pi.tail1234.ts.net:8443"), "POST", false), Context::SameSite, "one tailnet is one site");
        assert_eq!(of(Some("http://nas.tail9999.ts.net"), "POST", true), Context::CrossSite, "another tailnet is not");
        assert_eq!(of(Some("https://evil.example"), "GET", true), Context::CrossSiteNavigation);
        assert_eq!(of(Some("https://evil.example"), "GET", false), Context::CrossSite, "a frame is not a tab");
        assert_eq!(of(Some("https://evil.example"), "POST", true), Context::CrossSite);
        assert_eq!(of(Some("null"), "POST", true), Context::CrossSite);
        assert_eq!(of(None, "GET", true), Context::CrossSiteNavigation);

        let short = url("http://nas/");
        assert_eq!(Context::of(Some("http://nas:8080"), &short, "POST", false), Context::SameSite);
        assert_eq!(Context::of(Some("http://pi"), &short, "POST", false), Context::CrossSite);
        let address = url("http://100.64.0.1/");
        assert_eq!(Context::of(Some("http://100.64.0.1:81"), &address, "POST", false), Context::SameSite);
        assert_eq!(Context::of(Some("http://100.64.0.2"), &address, "POST", false), Context::CrossSite);
    }

    #[test]
    fn holds_same_site_cookies_back_from_other_sites() {
        let page = url("https://nas.tail1234.ts.net/");
        let jar = jar_with(
            "https://nas.tail1234.ts.net/",
            &["strict=1; SameSite=Strict", "lax=1; SameSite=Lax", "plain=1", "none=1; SameSite=None; Secure"],
        );
        let sent = |context| jar.header_in(&page, context);
        assert_eq!(sent(Context::SameSite).as_deref(), Some("lax=1; none=1; plain=1; strict=1"));
        assert_eq!(sent(Context::CrossSiteNavigation).as_deref(), Some("lax=1; none=1; plain=1"));
        assert_eq!(sent(Context::CrossSite).as_deref(), Some("none=1"));
    }
}
