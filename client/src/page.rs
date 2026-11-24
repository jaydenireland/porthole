//! What a page asks of the tailnet: requests, WebSockets and cookies.
//!
//! These calls have the same shapes as the ones Porthole's Go client gave the
//! app, so the app's bridge can use either:
//!
//! ```text
//! request({ url, method?, headers?: [name, value][], body?: Uint8Array,
//!           from?: string | null, topLevel?: boolean })
//!   -> Promise<{ status, statusText, url, headers: [name, value][],
//!                read(): Promise<Uint8Array | null>, cancel() }>
//! websocket({ url, protocols?: string[], headers?: [name, value][],
//!             onOpen(protocol), onMessage(string | Uint8Array),
//!             onClose(code, reason, wasClean), onError(message) })
//!   -> { send(string | Uint8Array): boolean, close(code?, reason?) }
//! cookies(url) -> string          setCookie(url, line)
//! exportCookies() -> string       importCookies(json)
//! ```

use std::{cell::RefCell, rc::Rc, time::Duration};

use futures_channel::mpsc;
use futures_util::{
    SinkExt, StreamExt,
    future::{Either, select},
};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{CloseFrame, frame::coding::CloseCode},
};
use url::Url;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

use crate::{
    cookies::Context,
    error::Error,
    http::{Body, Request},
    tab::{Client, Inner},
};

/// The close code for a socket that ended without a proper goodbye.
const ABNORMAL: u16 = 1006;
/// How long to wait for the other end to answer our goodbye.
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// Messages the page may queue before the socket is failed for being too slow.
const SEND_QUEUE: usize = 1024;

fn get(object: &JsValue, key: &str) -> JsValue {
    Reflect::get(object, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

fn set(object: &Object, key: &str, value: impl Into<JsValue>) {
    // Setting a property on a plain object we just made cannot fail.
    let _ = Reflect::set(object, &key.into(), &value.into());
}

/// Read `[[name, value], ...]`.
fn pairs(value: &JsValue) -> Vec<(String, String)> {
    if !Array::is_array(value) {
        return Vec::new();
    }
    Array::from(value)
        .iter()
        .filter_map(|pair| {
            let pair = Array::from(&pair);
            Some((pair.get(0).as_string()?, pair.get(1).as_string()?))
        })
        .collect()
}

fn strings(value: &JsValue) -> Vec<String> {
    if !Array::is_array(value) {
        return Vec::new();
    }
    Array::from(value).iter().filter_map(|v| v.as_string()).collect()
}

fn url_of(options: &JsValue, usage: &'static str) -> Result<Url, Error> {
    let text = get(options, "url").as_string().ok_or(Error::Usage(usage))?;
    Ok(Url::parse(&text)?)
}

/// A page function that outlives this call. The browser frees it when the
/// page lets go of it.
fn function<F: ?Sized + wasm_bindgen::closure::WasmClosure>(closure: Closure<F>) -> JsValue {
    closure.into_js_value()
}

#[wasm_bindgen]
impl Client {
    /// Send an HTTP request to a tailnet site. Redirects are not followed.
    ///
    /// `from` is the origin of the page that caused the request, such as
    /// `http://nas`, or `null` when it is not known. A request another site
    /// caused goes without the cookies marked `SameSite`; `topLevel` says it
    /// loads a page as a tab would, which lets `SameSite=Lax` cookies
    /// through on a GET. Leave `from` out for a request the site itself or
    /// the user made.
    /// Read the body with `read()` until it resolves to `null`, or call
    /// `cancel()`; either closes the connection.
    pub async fn request(&self, options: JsValue) -> Result<JsValue, Error> {
        const USAGE: &str = "request({url, method?, headers?, body?, from?, topLevel?})";
        let inner = self.inner.clone();
        let url = url_of(&options, USAGE)?;
        let body = get(&options, "body");
        let request = Request {
            method: get(&options, "method").as_string().unwrap_or_else(|| "GET".to_owned()),
            url: url.clone(),
            headers: pairs(&get(&options, "headers")),
            body: if body.is_instance_of::<Uint8Array>() {
                Uint8Array::from(body).to_vec()
            } else {
                Vec::new()
            },
        };

        let from = get(&options, "from");
        let context = if from.is_undefined() {
            Context::SameSite
        } else {
            let top_level = get(&options, "topLevel").is_truthy();
            Context::of(from.as_string().as_deref(), &url, &request.method, top_level)
        };

        let (head, body, _cookies_changed) = inner.site.request_in(request, context).await?;

        let headers = Array::new();
        for (name, value) in &head.headers {
            headers.push(&Array::of2(&name.into(), &value.into()));
        }
        // Taken out while a read is in progress, and for good at the end.
        let body: Rc<RefCell<Option<Body>>> = Rc::new(RefCell::new(Some(body)));

        let reading = body.clone();
        let read = Closure::<dyn FnMut() -> Promise>::new(move || {
            let reading = reading.clone();
            future_to_promise(async move {
                let Some(mut body) = reading.borrow_mut().take() else {
                    return Ok(JsValue::NULL);
                };
                match body.chunk().await {
                    Ok(Some(piece)) => {
                        *reading.borrow_mut() = Some(body);
                        Ok(Uint8Array::from(piece.as_ref()).into())
                    }
                    Ok(None) => Ok(JsValue::NULL),
                    Err(error) => Err(js_sys::Error::new(&error.to_string()).into()),
                }
            })
        });
        let cancel = Closure::<dyn FnMut()>::new(move || {
            body.borrow_mut().take();
        });

        let response = Object::new();
        set(&response, "status", head.status);
        set(&response, "statusText", head.status_text);
        set(&response, "url", url.as_str());
        set(&response, "headers", headers);
        set(&response, "read", function(read));
        set(&response, "cancel", function(cancel));
        Ok(response.into())
    }

    /// What `document.cookie` reads on a page at `url`.
    pub fn cookies(&self, url: String) -> String {
        match Url::parse(&url) {
            Ok(url) => self.inner.site.jar().borrow().script_cookies(&url),
            Err(_) => String::new(),
        }
    }

    /// What assigning `line` to `document.cookie` does on a page at `url`.
    #[wasm_bindgen(js_name = setCookie)]
    pub fn set_cookie(&self, url: String, line: String) {
        if let Ok(url) = Url::parse(&url) {
            self.inner.site.jar().borrow_mut().set_from_script(&url, &line);
        }
    }

    /// Every cookie, as JSON for the page to store: `[{ url, line, session, expires }]`.
    #[wasm_bindgen(js_name = exportCookies)]
    pub fn export_cookies(&self) -> String {
        self.inner.site.jar().borrow().export()
    }

    /// Load cookies saved by `exportCookies()`.
    #[wasm_bindgen(js_name = importCookies)]
    pub fn import_cookies(&self, json: String) {
        self.inner.site.jar().borrow_mut().import(&json);
    }

    /// Open a WebSocket to a tailnet site. `onClose` is always called exactly
    /// once, last.
    pub fn websocket(&self, options: JsValue) -> JsValue {
        const USAGE: &str = "websocket({url, protocols?, headers?, onOpen, onMessage, onClose, onError})";
        let (queue, queued) = mpsc::channel::<Outgoing>(SEND_QUEUE);
        let callbacks = Callbacks(options.clone());
        match url_of(&options, USAGE) {
            Ok(url) => spawn_local(run_socket(
                self.inner.clone(),
                url,
                strings(&get(&options, "protocols")),
                pairs(&get(&options, "headers")),
                callbacks,
                queued,
            )),
            Err(error) => {
                callbacks.call("onError", &[error.to_string().into()]);
                callbacks.closed(ABNORMAL, "", false);
            }
        }

        let sending = RefCell::new(queue.clone());
        let send = Closure::<dyn FnMut(JsValue) -> bool>::new(move |data: JsValue| {
            let message = if let Some(text) = data.as_string() {
                Message::text(text)
            } else if data.is_instance_of::<Uint8Array>() {
                Message::binary(Uint8Array::from(data).to_vec())
            } else {
                return false;
            };
            let mut queue = sending.borrow_mut();
            match queue.try_send(Outgoing::Message(message)) {
                Ok(()) => true,
                Err(_) => {
                    // The other end is not reading fast enough (or is gone).
                    // Fail the socket rather than hold messages without limit.
                    queue.close_channel();
                    false
                }
            }
        });
        let closing = RefCell::new(queue);
        let close = Closure::<dyn FnMut(JsValue, JsValue)>::new(move |code: JsValue, reason: JsValue| {
            let frame = code.as_f64().map(|code| CloseFrame {
                code: CloseCode::from(code as u16),
                reason: reason.as_string().unwrap_or_default().into(),
            });
            let _ = closing.borrow_mut().try_send(Outgoing::Close(frame));
        });

        let socket = Object::new();
        set(&socket, "send", function(send));
        set(&socket, "close", function(close));
        socket.into()
    }
}

enum Outgoing {
    Message(Message),
    Close(Option<CloseFrame>),
}

/// The page's `onOpen`, `onMessage`, `onClose` and `onError`.
struct Callbacks(JsValue);

impl Callbacks {
    fn call(&self, name: &str, arguments: &[JsValue]) {
        if let Ok(function) = get(&self.0, name).dyn_into::<Function>() {
            // A page callback that throws must not take the socket down.
            let _ = function.apply(&JsValue::NULL, &Array::from_iter(arguments.iter()));
        }
    }

    fn closed(&self, code: u16, reason: &str, clean: bool) {
        self.call("onClose", &[code.into(), reason.into(), clean.into()]);
    }
}

async fn run_socket(
    inner: Rc<Inner>,
    url: Url,
    protocols: Vec<String>,
    headers: Vec<(String, String)>,
    page: Callbacks,
    mut queued: mpsc::Receiver<Outgoing>,
) {
    let mut socket = match inner.site.websocket(&url, &protocols, &headers).await {
        Ok(((socket, protocol), _cookies_changed)) => {
            page.call("onOpen", &[protocol.unwrap_or_default().into()]);
            socket
        }
        Err(error) => {
            page.call("onError", &[error.to_string().into()]);
            page.closed(ABNORMAL, "", false);
            return;
        }
    };

    // Once we have said goodbye, how long the other end has to answer.
    let mut deadline: Option<std::pin::Pin<Box<crate::timer::Sleep>>> = None;
    loop {
        let incoming = socket.next();
        let event = match deadline.as_mut() {
            // After our goodbye nothing more is sent; wait for theirs or give up.
            Some(deadline) => match select(incoming, deadline).await {
                Either::Left((message, _)) => Either::Left(message),
                Either::Right(_) => {
                    page.closed(ABNORMAL, "", false);
                    return;
                }
            },
            None => match select(incoming, queued.next()).await {
                Either::Left((message, _)) => Either::Left(message),
                Either::Right((outgoing, _)) => Either::Right(outgoing),
            },
        };

        match event {
            Either::Left(Some(Ok(Message::Text(text)))) => {
                page.call("onMessage", &[text.as_str().into()]);
            }
            Either::Left(Some(Ok(Message::Binary(bytes)))) => {
                page.call("onMessage", &[Uint8Array::from(bytes.as_ref()).into()]);
            }
            Either::Left(Some(Ok(Message::Close(frame)))) => {
                let (code, reason) = match &frame {
                    Some(frame) => (u16::from(frame.code), frame.reason.to_string()),
                    // 1005: the goodbye carried no code.
                    None => (1005, String::new()),
                };
                // Let tungstenite finish the exchange before reporting.
                let _ = socket.flush().await;
                page.closed(code, &reason, true);
                return;
            }
            // Pings are answered by tungstenite.
            Either::Left(Some(Ok(_))) => {}
            Either::Left(Some(Err(error))) => {
                if deadline.is_none() {
                    page.call("onError", &[error.to_string().into()]);
                }
                page.closed(ABNORMAL, "", false);
                return;
            }
            Either::Left(None) => {
                page.closed(ABNORMAL, "", false);
                return;
            }
            Either::Right(Some(Outgoing::Message(message))) => {
                if let Err(error) = socket.send(message).await {
                    page.call("onError", &[error.to_string().into()]);
                    page.closed(ABNORMAL, "", false);
                    return;
                }
            }
            Either::Right(Some(Outgoing::Close(frame))) => {
                let _ = socket.close(frame).await;
                deadline = Some(Box::pin(crate::timer::sleep(CLOSE_WAIT)));
            }
            // The send queue overflowed and was shut.
            Either::Right(None) => {
                page.call("onError", &["the socket could not keep up with what was sent".into()]);
                page.closed(ABNORMAL, "", false);
                return;
            }
        }
    }
}
