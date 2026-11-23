//! One error type for everything the page can be told about.

use wasm_bindgen::JsValue;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the saved sign-in could not be read: {0}")]
    State(#[from] serde_json::Error),
    #[error("invalid address: {0}")]
    Url(#[from] url::ParseError),
    #[error("could not fetch the control server's key: {0}")]
    ControlKey(String),
    #[error("could not open a connection to the control server: {0}")]
    WebSocket(#[from] ws_stream_wasm::WsErr),
    #[error("the control server handshake failed: {0}")]
    Handshake(#[from] ts_control_noise::Error),
    #[error("the control connection failed: {0}")]
    Connection(String),
    #[error("registration failed: {0}")]
    Registration(String),
    #[error("this device is not signed in yet")]
    NotSignedIn,
    #[error("the device list could not be started: {0}")]
    Netmap(String),
    #[error("the relay connection failed: {0}")]
    Relay(String),
}

impl From<Error> for JsValue {
    fn from(error: Error) -> Self {
        js_sys::Error::new(&error.to_string()).into()
    }
}
