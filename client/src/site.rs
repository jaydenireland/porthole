//! Requests and WebSockets to tailnet sites: connect, add TLS when the
//! address asks for it, send, and keep the cookie jar up to date.
//!
//! How a connection to a device is opened is left to a [`Dialer`], so this
//! works over the tunnel in a browser and over an in-memory pipe in tests.

use std::{cell::RefCell, collections::HashMap, future::Future, io, rc::Rc, sync::Arc, time::Duration};

use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, client::IntoClientRequest, http::HeaderValue},
};
use url::Url;

use crate::{
    cookies::{Context, Jar},
    http::{self, Body, Connection, Head, Recycle, Request},
    tls,
};

pub trait Io: AsyncRead + AsyncWrite + Unpin {}
impl<T: AsyncRead + AsyncWrite + Unpin> Io for T {}

/// A connection to a device, with TLS on it or not.
pub type Stream = Box<dyn Io>;

/// Opens a plain TCP connection to a named device.
pub trait Dialer {
    fn dial(&self, host: &str, port: u16) -> impl Future<Output = io::Result<Stream>>;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} addresses are not supported")]
    Scheme(String),
    #[error("the address has no host")]
    NoHost,
    #[error("could not connect: {0}")]
    Connect(io::Error),
    #[error("could not set up a secure connection: {0}")]
    Tls(io::Error),
    #[error("{0}")]
    Http(#[from] http::Error),
    #[error("{0}")]
    WebSocket(#[from] tungstenite::Error),
}

/// How long an unused connection is kept. Servers close idle connections
/// after a while without saying so; a short limit means we rarely try one
/// that is already gone.
const IDLE_LIMIT: Duration = Duration::from_secs(30);
/// Unused connections kept per site. A page loads several things at once.
const IDLE_PER_SITE: usize = 8;

/// Scheme, host and port: what a connection can be reused for.
type Origin = (String, String, u16);

/// Open connections waiting for their next request.
#[derive(Default)]
struct Idle {
    connections: HashMap<Origin, Vec<(Connection, web_time::Instant)>>,
}

impl Idle {
    /// The most recently used connection to `origin` that is not too old.
    fn take(&mut self, origin: &Origin) -> Option<Connection> {
        let waiting = self.connections.get_mut(origin)?;
        while let Some((connection, since)) = waiting.pop() {
            if since.elapsed() < IDLE_LIMIT {
                return Some(connection);
            }
        }
        None
    }

    fn put(&mut self, origin: Origin, connection: Connection) {
        let waiting = self.connections.entry(origin).or_default();
        waiting.retain(|(_, since)| since.elapsed() < IDLE_LIMIT);
        if waiting.len() < IDLE_PER_SITE {
            waiting.push((connection, web_time::Instant::now()));
        }
    }
}

pub struct Site<D> {
    dialer: D,
    tls: Arc<ClientConfig>,
    jar: RefCell<Jar>,
    idle: Rc<RefCell<Idle>>,
}

/// An open WebSocket and the subprotocol the server chose, if any.
pub type Socket = (WebSocketStream<Stream>, Option<String>);

impl<D: Dialer> Site<D> {
    pub fn new(dialer: D, tls: Arc<ClientConfig>) -> Self {
        Self { dialer, tls, jar: RefCell::default(), idle: Rc::default() }
    }

    pub fn jar(&self) -> &RefCell<Jar> {
        &self.jar
    }

    /// Connect to the device `url` names, with TLS for `https` and `wss`.
    async fn open(&self, url: &Url) -> Result<Stream, Error> {
        let secure = match url.scheme() {
            "http" | "ws" => false,
            "https" | "wss" => true,
            other => return Err(Error::Scheme(other.to_owned())),
        };
        let host = url.host_str().ok_or(Error::NoHost)?;
        let port = url.port_or_known_default().ok_or(Error::NoHost)?;
        let stream = self.dialer.dial(host, port).await.map_err(Error::Connect)?;
        if !secure {
            return Ok(stream);
        }
        // IPv6 addresses are written in brackets in an address, but not in a certificate.
        let name = host.trim_start_matches('[').trim_end_matches(']');
        let stream = tls::connect(self.tls.clone(), name, stream).await.map_err(Error::Tls)?;
        Ok(Box::new(stream))
    }

    /// Send one request that the site itself or the user made. The site's
    /// cookies are added, and the ones it sets are kept; the last value says
    /// whether it set any.
    pub async fn request(&self, request: Request) -> Result<(Head, Body, bool), Error> {
        self.request_in(request, Context::SameSite).await
    }

    /// Send one request made in `context`, which decides the cookies added.
    pub async fn request_in(&self, mut request: Request, context: Context) -> Result<(Head, Body, bool), Error> {
        let url = request.url.clone();
        // Cookies come from the jar and nowhere else.
        request.headers.retain(|(name, _)| !name.eq_ignore_ascii_case("cookie"));
        if let Some(cookies) = self.jar.borrow().header_in(&url, context) {
            request.headers.push(("cookie".to_owned(), cookies));
        }
        let (head, body) = self.exchange(&url, request).await?;
        let set = head
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.as_str());
        let changed = self.jar.borrow_mut().store_response(&url, set);
        Ok((head, body, changed))
    }

    /// Send `request` on a connection left open by an earlier one if there
    /// is one, and on a new connection otherwise.
    async fn exchange(&self, url: &Url, request: Request) -> Result<(Head, Body), Error> {
        let origin: Origin = (
            url.scheme().to_owned(),
            url.host_str().ok_or(Error::NoHost)?.to_owned(),
            url.port_or_known_default().ok_or(Error::NoHost)?,
        );
        let recycle = || -> Recycle {
            let (idle, origin) = (self.idle.clone(), origin.clone());
            Box::new(move |connection| idle.borrow_mut().put(origin, connection))
        };

        // A connection that sat unused may have been closed by the server in
        // the meantime, which only shows when it is used. Browsers handle
        // that by sending the request again on a new connection, and so do we.
        let reused = self.idle.borrow_mut().take(&origin);
        if let Some(connection) = reused {
            match connection.send(request.clone(), Some(recycle())).await {
                Ok(response) => return Ok(response),
                Err(http::Error::Request(reason)) => return Err(http::Error::Request(reason).into()),
                Err(_stale) => {}
            }
        }
        let connection = http::connect(self.open(url).await?).await?;
        Ok(connection.send(request, Some(recycle())).await?)
    }

    /// Open a WebSocket, offering `protocols`. The site's cookies are sent,
    /// so a login made over HTTP also covers the socket.
    pub async fn websocket(
        &self,
        url: &Url,
        protocols: &[String],
        headers: &[(String, String)],
    ) -> Result<(Socket, bool), Error> {
        let invalid = |what: &str| {
            Error::WebSocket(tungstenite::Error::Url(
                tungstenite::error::UrlError::UnableToConnect(what.to_owned()),
            ))
        };
        let mut request = url.as_str().into_client_request()?;
        let fields = request.headers_mut();
        for (name, value) in headers {
            // The handshake's own headers are tungstenite's to set.
            let lower = name.to_ascii_lowercase();
            if lower == "cookie"
                || lower == "host"
                || lower == "connection"
                || lower == "upgrade"
                || lower.starts_with("sec-websocket-")
            {
                continue;
            }
            let name = tungstenite::http::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| invalid("a header name"))?;
            fields.append(name, HeaderValue::from_str(value).map_err(|_| invalid("a header value"))?);
        }
        if !protocols.is_empty() {
            let offered = HeaderValue::from_str(&protocols.join(", "))
                .map_err(|_| invalid("a subprotocol"))?;
            fields.insert("sec-websocket-protocol", offered);
        }
        if let Some(cookies) = self.jar.borrow().header_for(&as_http(url)) {
            fields.insert("cookie", HeaderValue::from_str(&cookies).map_err(|_| invalid("cookies"))?);
        }

        let stream = self.open(url).await?;
        let (socket, response) = tokio_tungstenite::client_async(request, stream).await?;
        let chosen = response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let set = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok());
        let changed = self.jar.borrow_mut().store_response(&as_http(url), set);
        Ok(((socket, chosen), changed))
    }
}

/// Cookies are kept under `http` and `https`; a socket uses the matching one.
fn as_http(url: &Url) -> Url {
    let scheme = if url.scheme() == "wss" { "https" } else { "http" };
    let mut url = url.clone();
    // Changing between these schemes cannot fail.
    let _ = url.set_scheme(scheme);
    url
}

#[cfg(test)]
mod tests {
    use futures_util::{SinkExt, StreamExt};
    use rustls::RootCertStore;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::http::tests::{local, request, serve, text};

    const HOST: &str = "nas.tail1234.ts.net";

    /// A device with a web site on 80, the same over TLS on 443, and an
    /// echoing WebSocket on 8080.
    struct Device {
        acceptor: tokio_rustls::TlsAcceptor,
        /// How many connections have been opened to it.
        dials: std::cell::Cell<usize>,
    }

    impl Dialer for Device {
        async fn dial(&self, host: &str, port: u16) -> io::Result<Stream> {
            if host != HOST {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such device"));
            }
            self.dials.set(self.dials.get() + 1);
            let (ours, theirs) = tokio::io::duplex(16 * 1024);
            match port {
                // A server that answers once, promises nothing, and hangs up.
                81 => {
                    tokio::task::spawn_local(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut theirs = theirs;
                        let mut request = Vec::new();
                        let mut piece = [0u8; 256];
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            match theirs.read(&mut piece).await {
                                Ok(got) if got > 0 => request.extend_from_slice(&piece[..got]),
                                _ => return,
                            }
                        }
                        let _ = theirs.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
                        // Dropping `theirs` closes the connection without a word.
                    });
                }
                80 => serve(theirs),
                443 => {
                    let acceptor = self.acceptor.clone();
                    tokio::task::spawn_local(async move {
                        if let Ok(stream) = acceptor.accept(theirs).await {
                            serve(stream);
                        }
                    });
                }
                8080 => {
                    tokio::task::spawn_local(async move {
                        let callback = |request: &tungstenite::handshake::server::Request,
                                        mut response: tungstenite::handshake::server::Response| {
                            let cookie = request.headers().get("cookie").cloned();
                            let headers = response.headers_mut();
                            headers.insert("sec-websocket-protocol", HeaderValue::from_static("chat"));
                            headers.insert("set-cookie", HeaderValue::from_static("ws=1; Path=/"));
                            if let Some(cookie) = cookie {
                                headers.insert("x-saw-cookie", cookie);
                            }
                            Ok(response)
                        };
                        let mut socket =
                            tokio_tungstenite::accept_hdr_async(theirs, callback).await.unwrap();
                        while let Some(Ok(message)) = socket.next().await {
                            if message.is_text() || message.is_binary() {
                                socket.send(message).await.unwrap();
                            }
                        }
                    });
                }
                _ => return Err(io::Error::new(io::ErrorKind::ConnectionRefused, "nothing listens there")),
            }
            Ok(Box::new(ours))
        }
    }

    /// The device, and a site handle that trusts its certificate.
    fn device() -> Site<Device> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let issued = rcgen::generate_simple_self_signed(vec![HOST.to_owned()]).unwrap();
        let certificate = CertificateDer::from(issued.cert.der().to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(issued.signing_key.serialize_der()));
        let server = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)
            .unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(certificate).unwrap();
        Site::new(
            Device { acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(server)), dials: Default::default() },
            tls::config_trusting(roots),
        )
    }

    #[tokio::test]
    async fn plain_request() {
        local(async {
            let site = device();
            let (head, body, _) =
                site.request(request("GET", &format!("http://{HOST}/echo"), &[], "")).await.unwrap();
            assert_eq!(head.status, 200);
            assert_eq!(text(body).await, format!("GET /echo host={HOST} x=- cookie=- body="));
        })
        .await;
    }

    #[tokio::test]
    async fn request_over_tls() {
        local(async {
            let site = device();
            let (head, body, _) = site
                .request(request("POST", &format!("https://{HOST}/echo"), &[], "secret"))
                .await
                .unwrap();
            assert_eq!(head.status, 200);
            assert_eq!(text(body).await, format!("POST /echo host={HOST} x=- cookie=- body=secret"));
        })
        .await;
    }

    #[tokio::test]
    async fn refuses_a_certificate_it_does_not_trust() {
        local(async {
            let trusted_elsewhere = device();
            // Same device, but checked against the real authorities.
            let site = Site::new(
                Device { acceptor: trusted_elsewhere.dialer.acceptor.clone(), dials: Default::default() },
                tls::config(),
            );
            let result = site.request(request("GET", &format!("https://{HOST}/echo"), &[], "")).await;
            assert!(matches!(result, Err(Error::Tls(_))));
        })
        .await;
    }

    #[tokio::test]
    async fn keeps_and_returns_cookies() {
        local(async {
            let site = device();
            let (_, _, changed) =
                site.request(request("GET", &format!("http://{HOST}/redirect"), &[], "")).await.unwrap();
            assert!(changed);
            // A cookie header from the page is ignored: the jar is the only source.
            let (_, body, changed) = site
                .request(request("GET", &format!("http://{HOST}/echo"), &[("Cookie", "forged=1")], ""))
                .await
                .unwrap();
            assert!(!changed);
            assert!(text(body).await.contains("cookie=a=1; b=2 "));
            let page = Url::parse(&format!("http://{HOST}/")).unwrap();
            assert_eq!(site.jar().borrow().script_cookies(&page), "a=1", "b is HttpOnly");
        })
        .await;
    }

    #[tokio::test]
    async fn websocket_echoes_and_shares_cookies() {
        local(async {
            let site = device();
            site.request(request("GET", &format!("http://{HOST}/redirect"), &[], "")).await.unwrap();

            let url = Url::parse(&format!("ws://{HOST}:8080/socket")).unwrap();
            let ((mut socket, protocol), changed) = site
                .websocket(&url, &["chat".to_owned(), "other".to_owned()], &[("Origin".to_owned(), format!("http://{HOST}"))])
                .await
                .unwrap();
            assert_eq!(protocol.as_deref(), Some("chat"));
            assert!(changed, "the handshake set a cookie");

            socket.send(Message::text("hello")).await.unwrap();
            assert_eq!(socket.next().await.unwrap().unwrap(), Message::text("hello"));
            socket.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
            assert_eq!(socket.next().await.unwrap().unwrap(), Message::binary(vec![1u8, 2, 3]));

            let page = Url::parse(&format!("http://{HOST}/")).unwrap();
            assert_eq!(site.jar().borrow().script_cookies(&page), "a=1; ws=1");
        })
        .await;
    }

    #[tokio::test]
    async fn reuses_a_connection_once_its_response_has_been_read() {
        local(async {
            let site = device();
            for scheme in ["http", "https"] {
                let before = site.dialer.dials.get();
                for _ in 0..3 {
                    let (_, body, _) =
                        site.request(request("GET", &format!("{scheme}://{HOST}/echo"), &[], "")).await.unwrap();
                    text(body).await;
                }
                assert_eq!(site.dialer.dials.get() - before, 1, "{scheme}: three requests, one connection");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn does_not_reuse_a_connection_with_an_unread_response() {
        local(async {
            let site = device();
            let (_, unread, _) =
                site.request(request("GET", &format!("http://{HOST}/echo"), &[], "")).await.unwrap();
            let (_, body, _) =
                site.request(request("GET", &format!("http://{HOST}/echo"), &[], "")).await.unwrap();
            text(body).await;
            drop(unread);
            assert_eq!(site.dialer.dials.get(), 2);
        })
        .await;
    }

    #[tokio::test]
    async fn sends_again_when_a_kept_connection_has_been_closed() {
        local(async {
            let site = device();
            for _ in 0..2 {
                let (head, body, _) =
                    site.request(request("POST", &format!("http://{HOST}:81/"), &[], "data")).await.unwrap();
                assert_eq!((head.status, text(body).await.as_str()), (200, "ok"));
                // Let the server's side of the connection finish closing.
                tokio::task::yield_now().await;
            }
            assert_eq!(site.dialer.dials.get(), 2, "the second request needed a new connection");
        })
        .await;
    }

    #[tokio::test]
    async fn unpacks_gzip_it_asked_for() {
        local(async {
            let site = device();
            let (head, body, _) =
                site.request(request("GET", &format!("http://{HOST}/gzip"), &[], "")).await.unwrap();
            assert!(!head.headers.iter().any(|(name, _)| name == "content-encoding" || name == "content-length"));
            assert_eq!(text(body).await, "compressed ".repeat(500));
            // And the connection is still good afterwards.
            let (_, body, _) =
                site.request(request("GET", &format!("http://{HOST}/echo"), &[], "")).await.unwrap();
            assert!(text(body).await.starts_with("GET /echo"));
            assert_eq!(site.dialer.dials.get(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn leaves_alone_an_encoding_the_page_chose() {
        local(async {
            let site = device();
            let (head, mut body, _) = site
                .request(request("GET", &format!("http://{HOST}/gzip"), &[("Accept-Encoding", "gzip")], ""))
                .await
                .unwrap();
            assert!(head.headers.iter().any(|(name, value)| name == "content-encoding" && value == "gzip"));
            let first = body.chunk().await.unwrap().unwrap();
            assert_eq!(&first[..2], &[0x1f, 0x8b], "still gzip");
        })
        .await;
    }

    #[tokio::test]
    async fn refuses_other_kinds_of_address() {
        local(async {
            let site = device();
            let result = site.request(request("GET", "ftp://nas.tail1234.ts.net/", &[], "")).await;
            assert!(matches!(result, Err(Error::Scheme(_))));
            let result = site.request(request("GET", "http://elsewhere/", &[], "")).await;
            assert!(matches!(result, Err(Error::Connect(_))));
        })
        .await;
    }
}
