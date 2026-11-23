//! One HTTP/1.1 request over a connection the caller supplies.
//!
//! hyper does the protocol. This module shapes it for a page: the status and
//! headers come back as soon as they arrive, and the body is read a piece at
//! a time, which is what lets large downloads and never-ending responses
//! (server-sent events) through. Redirects are not followed, because the
//! page's service worker has to see them.
//!
//! Each request uses its own connection, which is closed afterwards.

use std::{future::Future, pin::Pin};

use bytes::Bytes;
use futures_util::future::{Either, select};
use http::{HeaderName, HeaderValue, Method, header};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the request is not valid: {0}")]
    Request(String),
    #[error("{0}")]
    Connection(#[from] hyper::Error),
}

pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The start of a response.
#[derive(Debug)]
pub struct Head {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
}

/// Keeps the connection moving. hyper splits a connection into the part that
/// sends requests and a task that does the reading and writing; rather than
/// hand that task to some runtime, the body drives it whenever it is read.
type Driver = Pin<Box<dyn Future<Output = Result<(), hyper::Error>>>>;

/// The rest of a response. Dropping it closes the connection.
pub struct Body {
    incoming: Incoming,
    driver: Option<Driver>,
}

/// Headers that describe the connection, not the request. We set these.
fn is_ours(name: &HeaderName) -> bool {
    [header::HOST, header::CONNECTION, header::CONTENT_LENGTH, header::TRANSFER_ENCODING]
        .contains(name)
        || name == "keep-alive"
        || name == "proxy-connection"
        || name == "upgrade"
}

/// The `Host` header for `url`: the port is left out when it is the usual one.
pub fn host_header(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn build(request: Request) -> Result<http::Request<Full<Bytes>>, Error> {
    let bad = |what: &str| Error::Request(what.to_owned());
    let method = Method::from_bytes(request.method.as_bytes()).map_err(|_| bad("method"))?;
    let host = host_header(&request.url).ok_or_else(|| bad("the address has no host"))?;
    let mut target = request.url.path().to_owned();
    if let Some(query) = request.url.query() {
        target.push('?');
        target.push_str(query);
    }

    let mut built = http::Request::builder()
        .method(method)
        .uri(target)
        .body(Full::new(Bytes::from(request.body)))
        .map_err(|e| Error::Request(e.to_string()))?;
    let headers = built.headers_mut();
    headers.insert(header::HOST, HeaderValue::from_str(&host).map_err(|_| bad("host"))?);
    headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    for (name, value) in request.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| Error::Request(format!("header name {name:?}")))?;
        if is_ours(&name) {
            continue;
        }
        let value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|_| Error::Request(format!("value of header {name}")))?;
        headers.append(name, value);
    }
    Ok(built)
}

/// Send `request` over `stream` and wait for the start of the response.
pub async fn send<Io>(stream: Io, request: Request) -> Result<(Head, Body), Error>
where
    Io: AsyncRead + AsyncWrite + Unpin + 'static,
{
    let request = build(request)?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    let driver: Driver = Box::pin(connection);

    let (response, driver) = match select(Box::pin(sender.send_request(request)), driver).await {
        Either::Left((response, driver)) => (response?, Some(driver)),
        // The connection finished first, which it can when the whole response
        // has already arrived; the response is then ready too. An error from
        // the connection at this point may only be about closing it (the
        // other end hung up first), so it counts only if no response came.
        Either::Right((done, response)) => match response.await {
            Ok(response) => (response, None),
            Err(error) => return Err(done.err().unwrap_or(error).into()),
        },
    };

    let (parts, incoming) = response.into_parts();
    let head = Head {
        status: parts.status.as_u16(),
        status_text: parts.status.canonical_reason().unwrap_or("").to_owned(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned())
            })
            .collect(),
    };
    Ok((head, Body { incoming, driver }))
}

impl Body {
    /// The next piece of the body, or `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        loop {
            let frame = match self.driver.take() {
                Some(driver) => match select(self.incoming.frame(), driver).await {
                    Either::Left((frame, driver)) => {
                        self.driver = Some(driver);
                        frame
                    }
                    // The connection is done; what it read is still waiting.
                    Either::Right((_done, frame)) => frame.await,
                },
                None => self.incoming.frame().await,
            };
            match frame {
                None => return Ok(None),
                Some(Err(error)) => return Err(error.into()),
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) if !data.is_empty() => return Ok(Some(data)),
                    // An empty piece, or trailing headers, which pages cannot see.
                    _ => {}
                },
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::convert::Infallible;

    use http_body_util::{BodyExt, StreamBody};
    use hyper::{body::Frame, service::service_fn};

    use super::*;

    type ServerBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

    fn full(text: impl Into<Bytes>) -> ServerBody {
        Full::new(text.into()).boxed()
    }

    /// A small site: echoes requests, redirects, and streams.
    async fn site(request: http::Request<Incoming>) -> Result<http::Response<ServerBody>, Infallible> {
        let (parts, body) = request.into_parts();
        let body = body.collect().await.unwrap().to_bytes();
        let response = match parts.uri.path() {
            "/echo" => {
                let header = |name: &str| {
                    parts.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("-").to_owned()
                };
                let text = format!(
                    "{} {} host={} x={} cookie={} body={}",
                    parts.method,
                    parts.uri,
                    header("host"),
                    header("x-test"),
                    header("cookie"),
                    String::from_utf8_lossy(&body),
                );
                http::Response::new(full(text))
            }
            "/redirect" => http::Response::builder()
                .status(302)
                .header("location", "/echo")
                .header("set-cookie", "a=1; Path=/")
                .header("set-cookie", "b=2; Path=/; HttpOnly")
                .body(full(""))
                .unwrap(),
            "/stream" => {
                let pieces = futures_util::stream::unfold(0u8, |n| async move {
                    if n == 3 {
                        return None;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    Some((Ok(Frame::data(Bytes::from(format!("piece {n}\n")))), n + 1))
                });
                http::Response::new(StreamBody::new(pieces).boxed())
            }
            _ => http::Response::builder().status(404).body(full("not found")).unwrap(),
        };
        Ok(response)
    }

    /// Serve the test site on one end of an in-memory connection.
    pub(crate) fn serve<Io>(stream: Io)
    where
        Io: AsyncRead + AsyncWrite + Unpin + 'static,
    {
        tokio::task::spawn_local(async move {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service_fn(site))
                .await;
        });
    }

    pub(crate) fn request(method: &str, url: &str, headers: &[(&str, &str)], body: &str) -> Request {
        Request {
            method: method.to_owned(),
            url: Url::parse(url).unwrap(),
            headers: headers.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    pub(crate) async fn text(mut body: Body) -> String {
        let mut all = Vec::new();
        while let Some(piece) = body.chunk().await.unwrap() {
            all.extend_from_slice(&piece);
        }
        String::from_utf8(all).unwrap()
    }

    /// Run `test` where `spawn_local` works.
    pub(crate) async fn local<F: Future>(test: F) -> F::Output {
        tokio::task::LocalSet::new().run_until(test).await
    }

    #[tokio::test]
    async fn sends_method_path_headers_and_body() {
        local(async {
            let (ours, theirs) = tokio::io::duplex(1024);
            serve(theirs);
            let request = request(
                "POST",
                "http://nas.example.ts.net:8080/echo?q=1",
                &[("X-Test", "yes"), ("Host", "evil"), ("Connection", "keep-alive")],
                "hello",
            );
            let (head, body) = send(ours, request).await.unwrap();
            assert_eq!((head.status, head.status_text.as_str()), (200, "OK"));
            assert_eq!(
                text(body).await,
                "POST /echo?q=1 host=nas.example.ts.net:8080 x=yes cookie=- body=hello"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn reports_a_redirect_without_following_it() {
        local(async {
            let (ours, theirs) = tokio::io::duplex(1024);
            serve(theirs);
            let (head, _body) =
                send(ours, request("GET", "http://nas/redirect", &[], "")).await.unwrap();
            assert_eq!(head.status, 302);
            let named = |name: &str| -> Vec<&str> {
                head.headers.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str()).collect()
            };
            assert_eq!(named("location"), ["/echo"]);
            assert_eq!(named("set-cookie"), ["a=1; Path=/", "b=2; Path=/; HttpOnly"]);
        })
        .await;
    }

    #[tokio::test]
    async fn hands_over_the_body_as_it_arrives() {
        local(async {
            let (ours, theirs) = tokio::io::duplex(1024);
            serve(theirs);
            let (_head, mut body) =
                send(ours, request("GET", "http://nas/stream", &[], "")).await.unwrap();
            let mut pieces = Vec::new();
            while let Some(piece) = body.chunk().await.unwrap() {
                pieces.push(String::from_utf8(piece.to_vec()).unwrap());
            }
            assert_eq!(pieces, ["piece 0\n", "piece 1\n", "piece 2\n"]);
        })
        .await;
    }

    #[tokio::test]
    async fn refuses_a_bad_method() {
        let (ours, _theirs) = tokio::io::duplex(64);
        let result = send(ours, request("NOT A METHOD", "http://nas/", &[], "")).await;
        assert!(matches!(result, Err(Error::Request(_))));
    }
}
