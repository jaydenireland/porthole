//! Waiting for an amount of time.
//!
//! The obvious way, `setTimeout` on the page, has a catch: browsers slow a
//! page's timers down when its tab is not visible, to once a second or far
//! less. The TCP stack and WireGuard both wait on timers, so a page loading
//! in a background tab would stall. Timers in a worker are not slowed the
//! same way, so the waiting is done in a small one, and the page's own
//! timers are the fallback where a worker cannot be started.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(inline_js = r#"
let worker;            // undefined: not tried yet; null: unavailable
let next = 1;
const waiting = new Map();  // id -> { ms, resolve, promise, handle }

function fire(id) {
  const entry = waiting.get(id);
  if (entry) { waiting.delete(id); entry.resolve(); }
}

function timerWorker() {
  if (worker !== undefined) return worker;
  try {
    const source = `
      const timers = new Map();
      onmessage = (ev) => {
        const [id, ms] = ev.data;
        if (ms < 0) { clearTimeout(timers.get(id)); timers.delete(id); return; }
        timers.set(id, setTimeout(() => { timers.delete(id); postMessage(id); }, ms));
      };`;
    worker = new Worker(URL.createObjectURL(new Blob([source], { type: "text/javascript" })));
    worker.onmessage = (ev) => fire(ev.data);
    worker.onerror = () => {
      // Carry on with the page's own timers, for everything still waiting.
      worker = null;
      for (const [id, entry] of waiting) entry.handle = setTimeout(() => fire(id), entry.ms);
    };
  } catch {
    worker = null;
  }
  return worker;
}

export function porthole_timer_start(ms) {
  const id = next++;
  const entry = { ms };
  entry.promise = new Promise((resolve) => { entry.resolve = resolve; });
  waiting.set(id, entry);
  const w = timerWorker();
  if (w) w.postMessage([id, ms]);
  else entry.handle = setTimeout(() => fire(id), ms);
  return id;
}

export function porthole_timer_done(id) {
  const entry = waiting.get(id);
  return entry ? entry.promise : Promise.resolve();
}

export function porthole_timer_cancel(id) {
  const entry = waiting.get(id);
  if (!entry) return;
  waiting.delete(id);
  if (worker) worker.postMessage([id, -1]);
  else clearTimeout(entry.handle);
}
"#)]
extern "C" {
    fn porthole_timer_start(ms: f64) -> f64;
    fn porthole_timer_done(id: f64) -> js_sys::Promise;
    fn porthole_timer_cancel(id: f64);
}

/// Resolves after a while. Dropping it early cancels the timer.
pub struct Sleep {
    id: f64,
    done: JsFuture,
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.done).poll(cx).map(|_| ())
    }
}

impl Drop for Sleep {
    fn drop(&mut self) {
        // Does nothing if the timer has already gone off.
        porthole_timer_cancel(self.id);
    }
}

pub fn sleep(time: Duration) -> Sleep {
    let id = porthole_timer_start(time.as_secs_f64() * 1000.0);
    Sleep { id, done: JsFuture::from(porthole_timer_done(id)) }
}

pub fn sleep_ms(ms: u32) -> Sleep {
    sleep(Duration::from_millis(ms.into()))
}
