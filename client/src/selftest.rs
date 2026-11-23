//! Checks that run in the browser with no sign-in, on two throwaway devices
//! created in the tab. They meet at a real Tailscale relay and talk to each
//! other the way the tab talks to a real device, so everything between the
//! page and the other device is exercised except the tailnet itself.
//!
//! Built only with the `diagnostics` feature.

use std::{collections::HashMap, io, net::{IpAddr, SocketAddr}, rc::Rc, sync::Arc};

use futures_util::{SinkExt, StreamExt, future::{AbortHandle, abortable}};
use rustls::RootCertStore;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::compat::FuturesAsyncReadCompatExt;
use ts_keys::NodeKeyPair;
use ts_netstack_smoltcp::netsock::{TcpListener, TcpStream};
use url::Url;

use crate::{
    engine::{Engine, Peer},
    error::Error,
    http::Request,
    site::{Dialer, Site, Stream},
    tls,
};

/// The name the second device answers to.
const NAME: &str = "device-b.test";

/// Two devices that know each other and are both connected to the relay.
struct Pair {
    a: Engine,
    b: Engine,
    address_b: IpAddr,
}

async fn pair(relay: &str) -> Result<Pair, Error> {
    const REGION: u32 = 1;
    let (key_a, key_b) = (NodeKeyPair::random(), NodeKeyPair::random());
    let address_a: IpAddr = "100.64.0.1".parse().unwrap();
    let address_b: IpAddr = "100.64.0.2".parse().unwrap();
    let as_peer = |id, key: &NodeKeyPair, address| Peer {
        id,
        key: key.public,
        addresses: vec![address],
        relay_region: Some(REGION),
    };

    let a = Engine::new(key_a.clone());
    let b = Engine::new(key_b.clone());
    for (engine, address, other) in [
        (&a, address_a, as_peer(2, &key_b, address_b)),
        (&b, address_b, as_peer(1, &key_a, address_a)),
    ] {
        engine.set_relays(HashMap::from([(REGION, relay.to_owned())]));
        engine.set_addresses(vec![address]).await?;
        engine.set_peers(&[other]);
        engine.listen_on(REGION).await?;
    }
    Ok(Pair { a, b, address_b })
}

fn failed(what: &str) -> Error {
    Error::Network(what.to_owned())
}

/// WireGuard and TCP: one device opens a connection to the other and gets an
/// answer back.
pub async fn path(relay: &str) -> Result<(), Error> {
    let Pair { a, b, address_b } = pair(relay).await?;
    let listener = b.listen(80).await?;
    let server = async {
        let stream = listener.accept().await.map_err(|e| Error::Network(e.to_string()))?;
        let mut request = [0u8; 4];
        read_exact(&stream, &mut request).await?;
        if &request != b"ping" {
            return Err(failed("the listener received something else"));
        }
        write_all(&stream, b"pong").await
    };
    let client = async {
        let stream = a.connect(SocketAddr::new(address_b, 80)).await?;
        write_all(&stream, b"ping").await?;
        let mut answer = [0u8; 4];
        read_exact(&stream, &mut answer).await?;
        if &answer != b"pong" {
            return Err(failed("the answer was something else"));
        }
        Ok(())
    };
    let (served, answered) = futures_util::future::join(server, client).await;
    served.and(answered)
}

/// The device list: feeds an update shaped like the control server's through
/// the same code a real one takes, and returns what the page would be shown.
/// The devices are in the states a real tailnet has them in: online, offline
/// with and without a last-seen time, and unknown.
pub async fn netmap() -> Result<String, Error> {
    use ts_keys::{DiscoKeyPair, MachineKeyPair};

    let device = |id: u32, name: &str, extra: &str| {
        format!(
            r#"{{"ID":{id},"StableID":"n{id}","Name":"{name}.tail1234.ts.net.","Key":"{}","Machine":"{}","DiscoKey":"{}","Addresses":["100.64.0.{id}/32","fd7a:115c:a1e0::{id}/128"],"AllowedIPs":["100.64.0.{id}/32","fd7a:115c:a1e0::{id}/128"],"HomeDERP":1,"Cap":130,"MachineAuthorized":true,"Created":"2026-01-01T00:00:00Z"{extra}}}"#,
            NodeKeyPair::random().public,
            MachineKeyPair::random().public,
            DiscoKeyPair::random().public,
        )
    };
    let update = format!(
        r#"{{"Node":{},"Peers":[{},{},{},{}],"DERPMap":{{"Regions":{{"1":{{"RegionID":1,"RegionCode":"nyc","RegionName":"New York City","Nodes":[{{"Name":"1f","RegionID":1,"HostName":"derp1.tailscale.com"}}]}}}}}},"PacketFilter":[{{"SrcIPs":["*"],"DstPorts":[{{"IP":"*","Ports":{{"First":0,"Last":65535}}}}]}}],"Domain":"example.com"}}"#,
        device(1, "porthole", r#","Online":true"#),
        device(2, "nas", r#","Online":true"#),
        device(3, "laptop", r#","Online":false"#),
        device(4, "phone", r#","Online":false,"LastSeen":"2026-11-01T12:00:00Z""#),
        device(5, "pi", ""),
    );
    // Parsed once directly, so a mistake in the update above is reported
    // with its reason instead of as a silent end of the stream.
    serde_json::from_str::<ts_control_serde::MapResponse>(&update)
        .map_err(|e| Error::Netmap(format!("the test update does not parse: {e}")))?;

    let mut framed = (update.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(update.as_bytes());
    let mut updates = std::pin::pin!(ts_control::client::map_stream(io::Cursor::new(framed)));
    let first = updates.next().await.ok_or_else(|| failed("the update produced nothing"))?;

    let mut netmap = crate::netmap::Netmap::default();
    netmap.apply(first);
    // Everything the client does with a device list, on an engine of its own.
    let engine = Engine::new(NodeKeyPair::random());
    engine.set_relays(netmap.relay_hosts());
    engine.set_peers(&netmap.engine_peers());
    engine
        .set_addresses(netmap.addresses().ok_or_else(|| failed("the update had no addresses for this device"))?)
        .await?;
    if netmap.resolve("nas").is_none() || netmap.resolve("NAS.tail1234.ts.net").is_none() {
        return Err(failed("a device could not be found by name"));
    }
    netmap.to_json().ok_or_else(|| failed("there was no device list to show"))
}

/// Recovery: both devices lose their relay connections, as they would when
/// a phone changes networks, and must find each other again by themselves.
pub async fn recovery(relay: &str) -> Result<(), Error> {
    let Pair { a, b, address_b } = pair(relay).await?;
    let listener = b.listen(80).await?;
    let exchange = || async {
        let server = async {
            let stream = listener.accept().await.map_err(|e| Error::Network(e.to_string()))?;
            let mut request = [0u8; 4];
            read_exact(&stream, &mut request).await?;
            write_all(&stream, b"pong").await
        };
        let client = async {
            let stream = a.connect(SocketAddr::new(address_b, 80)).await?;
            write_all(&stream, b"ping").await?;
            let mut answer = [0u8; 4];
            read_exact(&stream, &mut answer).await?;
            if &answer == b"pong" { Ok(()) } else { Err(failed("the answer was something else")) }
        };
        let (served, answered) = futures_util::future::join(server, client).await;
        served.and(answered)
    };
    exchange().await?;
    a.cut_relays();
    b.cut_relays();
    // Nothing tells the devices to reconnect; the next connection just has
    // to work, once both are back on the relay.
    exchange().await
}

/// Reaches the second device whatever name is asked for.
struct Direct {
    engine: Rc<Engine>,
    address: IpAddr,
}

impl Dialer for Direct {
    async fn dial(&self, _host: &str, port: u16) -> io::Result<Stream> {
        let stream = self
            .engine
            .connect(SocketAddr::new(self.address, port))
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Box::new(stream.compat()))
    }
}

/// Stops the second device's servers when the check ends.
struct Servers(Vec<AbortHandle>);

impl Drop for Servers {
    fn drop(&mut self) {
        for server in &self.0 {
            server.abort();
        }
    }
}

impl Servers {
    /// Hand every connection `listener` accepts to `serve`.
    fn run<F>(&mut self, listener: TcpListener, serve: impl Fn(TcpStream) -> F + 'static)
    where
        F: Future<Output = ()> + 'static,
    {
        let (accepting, handle) = abortable(async move {
            while let Ok(stream) = listener.accept().await {
                wasm_bindgen_futures::spawn_local(serve(stream));
            }
        });
        self.0.push(handle);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = accepting.await;
        });
    }
}

/// The smallest web server: answers any request with `hello`, sets a cookie,
/// and says whether that cookie came back. `/page` is an HTML page.
async fn answer(mut stream: impl AsyncRead + AsyncWrite + Unpin) {
    let mut request = Vec::new();
    let mut piece = [0u8; 512];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut piece).await {
            Ok(got) if got > 0 => request.extend_from_slice(&piece[..got]),
            _ => return,
        }
    }
    let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
    // "/page" is a web page that tells the window showing it what it sees.
    let (kind, body) = if request.starts_with("get /page") {
        ("text/html; charset=utf-8", "<!doctype html><title>device b</title><script>top.postMessage({ type: 'device-b-page', cookie: document.cookie, host: location.host }, '*')</script>hello")
    } else if request.contains("cookie: seen=1") {
        ("text/plain", "hello again")
    } else {
        ("text/plain", "hello")
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nSet-Cookie: seen=1; Path=/\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// A web page on port 80 and an echoing WebSocket on port 8080.
async fn plain_servers(device: &Engine) -> Result<Servers, Error> {
    let mut servers = Servers(Vec::new());
    servers.run(device.listen(80).await?, |stream| answer(stream.compat()));
    servers.run(device.listen(8080).await?, |stream| async move {
        let Ok(mut socket) = tokio_tungstenite::accept_async(stream.compat()).await else { return };
        while let Some(Ok(message)) = socket.next().await {
            if (message.is_text() || message.is_binary()) && socket.send(message).await.is_err() {
                return;
            }
        }
    });
    Ok(servers)
}

/// A client whose whole network is the second device, for trying the calls
/// pages make.
pub async fn paired_client(relay: &str) -> Result<crate::tab::Client, Error> {
    let Pair { a, b, address_b } = pair(relay).await?;
    let servers = plain_servers(&b).await?;
    let mut netmap = crate::netmap::Netmap::default();
    netmap.alias(NAME, address_b);
    let net = Rc::new(crate::tab::Net { netmap: std::cell::RefCell::new(netmap), engine: a });
    Ok(crate::tab::Client::outside_a_tailnet(net, Box::new((b, servers))))
}

/// What the page layer does, between the two devices: an HTTP request, the
/// same over TLS with the certificate checked, a cookie kept and sent back,
/// and a WebSocket.
pub async fn site(relay: &str) -> Result<(), Error> {
    let Pair { a, b, address_b } = pair(relay).await?;

    // A certificate for the second device, trusted only by this check.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let issued = rcgen::generate_simple_self_signed(vec![NAME.to_owned()])
        .map_err(|e| Error::Network(e.to_string()))?;
    let certificate = CertificateDer::from(issued.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(issued.signing_key.serialize_der()));
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_no_client_auth().with_single_cert(vec![certificate.clone()], key))
        .map_err(|e| Error::Network(e.to_string()))?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let mut roots = RootCertStore::empty();
    roots.add(certificate).map_err(|e| Error::Network(e.to_string()))?;

    let mut servers = plain_servers(&b).await?;
    servers.run(b.listen(443).await?, move |stream| {
        let acceptor = acceptor.clone();
        async move {
            if let Ok(stream) = acceptor.accept(stream.compat()).await {
                answer(stream).await;
            }
        }
    });

    let site = Site::new(Direct { engine: Rc::new(a), address: address_b }, tls::config_trusting(roots));
    let get = |address: String| Request {
        method: "GET".to_owned(),
        url: Url::parse(&address).expect("a fixed address"),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let text = |mut body: crate::http::Body| async move {
        let mut all = Vec::new();
        while let Some(piece) = body.chunk().await.map_err(|e| Error::Network(e.to_string()))? {
            all.extend_from_slice(&piece);
        }
        Ok::<_, Error>(String::from_utf8_lossy(&all).into_owned())
    };

    let (head, body, cookie_set) = site.request(get(format!("http://{NAME}/"))).await?;
    if head.status != 200 || text(body).await? != "hello" || !cookie_set {
        return Err(failed("the plain request got a different answer"));
    }
    let (head, body, _) = site.request(get(format!("https://{NAME}/"))).await?;
    if head.status != 200 || text(body).await? != "hello again" {
        return Err(failed("the secure request got a different answer, or the cookie was not sent"));
    }

    let address = Url::parse(&format!("ws://{NAME}:8080/")).expect("a fixed address");
    let ((mut socket, _protocol), _) = site.websocket(&address, &[], &[]).await?;
    socket.send(Message::text("ping")).await.map_err(|e| Error::Network(e.to_string()))?;
    match socket.next().await {
        Some(Ok(message)) if message == Message::text("ping") => {}
        _ => return Err(failed("the socket did not echo")),
    }
    let _ = socket.close(None).await;
    Ok(())
}

async fn write_all(stream: &TcpStream, mut bytes: &[u8]) -> Result<(), Error> {
    while !bytes.is_empty() {
        let sent = stream.send(bytes).await.map_err(|e| Error::Network(e.to_string()))?;
        bytes = &bytes[sent..];
    }
    Ok(())
}

async fn read_exact(stream: &TcpStream, mut into: &mut [u8]) -> Result<(), Error> {
    while !into.is_empty() {
        let got = stream.recv(into).await.map_err(|e| Error::Network(e.to_string()))?;
        if got == 0 {
            return Err(Error::Network("the connection closed early".into()));
        }
        into = &mut into[got..];
    }
    Ok(())
}
