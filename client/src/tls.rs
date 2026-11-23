//! TLS for `https://` and `wss://` sites on the tailnet.
//!
//! The browser cannot do this for us: the connection runs inside the tunnel,
//! where the browser never sees it. rustls does the protocol, ring the
//! cryptography, and the certificates are checked against Mozilla's list of
//! root authorities (the `webpki-roots` crate), which is what makes a
//! `*.ts.net` certificate from Let's Encrypt trusted here.

use std::{io, sync::Arc};

use rustls::{ClientConfig, RootCertStore, time_provider::TimeProvider};
use rustls_pki_types::{ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::{TlsConnector, client::TlsStream};

/// The clock certificates are checked against. rustls would use the standard
/// library's, which does not exist in a browser.
#[derive(Debug)]
struct Clock;

impl TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).ok()?;
        Some(UnixTime::since_unix_epoch(now))
    }
}

/// Settings for connections to sites: trust `roots`, speak HTTP/1.1.
pub fn config_trusting(roots: RootCertStore) -> Arc<ClientConfig> {
    let mut config = ClientConfig::builder_with_details(
        Arc::new(rustls::crypto::ring::default_provider()),
        Arc::new(Clock),
    )
    .with_safe_default_protocol_versions()
    .expect("ring supports rustls's default TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

/// Settings that trust the same certificate authorities as Firefox.
pub fn config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    config_trusting(roots)
}

/// Start TLS on `stream` and check that the other end is `host`.
pub async fn connect<Io>(
    config: Arc<ClientConfig>,
    host: &str,
    stream: Io,
) -> io::Result<TlsStream<Io>>
where
    Io: AsyncRead + AsyncWrite + Unpin,
{
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a valid host name"))?;
    TlsConnector::from(config).connect(name, stream).await
}
