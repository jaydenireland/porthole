//! Requests and WebSockets to tailnet sites: connect, add TLS when the
//! address asks for it, send, and keep the cookie jar up to date.
//!
//! How a connection to a device is opened is left to a [`Dialer`], so this
//! works over the tunnel in a browser and over an in-memory pipe in tests.

use std::{cell::RefCell, future::Future, io, sync::Arc};

use rustls::ClientConfig;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, client::IntoClientRequest, http::HeaderValue},
};
use url::Url;

use crate::{
    cookies::Jar,
    http::{self, Body, Head, Request},
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

pub struct Site<D> {
    dialer: D,
    tls: Arc<ClientConfig>,
    jar: RefCell<Jar>,
}

/// An open WebSocket and the subprotocol the server chose, if any.
pub type Socket = (WebSocketStream<Stream>, Option<String>);

impl<D: Dialer> Site<D> {
    pub fn new(dialer: D, tls: Arc<ClientConfig>) -> Self {
        Self { dialer, tls, jar: RefCell::default() }
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

    /// Send one request. The site's cookies are added, and the ones it sets
    /// are kept; the last value says whether it set any.
    pub async fn request(&self, mut request: Request) -> Result<(Head, Body, bool), Error> {
        let url = request.url.clone();
        // Cookies come from the jar and nowhere else.
        request.headers.retain(|(name, _)| !name.eq_ignore_ascii_case("cookie"));
        if let Some(cookies) = self.jar.borrow().header_for(&url) {
            request.headers.push(("cookie".to_owned(), cookies));
        }
        let stream = self.open(&url).await?;
        let (head, body) = http::send(stream, request).await?;
        let set = head
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.as_str());
        let changed = self.jar.borrow_mut().store_response(&url, set);
        Ok((head, body, changed))
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
    }

    impl Dialer for Device {
        async fn dial(&self, host: &str, port: u16) -> io::Result<Stream> {
            if host != HOST {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no such device"));
            }
            let (ours, theirs) = tokio::io::duplex(16 * 1024);
            match port {
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
            Device { acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(server)) },
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
            let site = Site::new(Device { acceptor: trusted_elsewhere.dialer.acceptor.clone() }, tls::config());
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
