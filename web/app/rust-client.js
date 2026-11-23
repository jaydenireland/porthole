// The Rust build of the Tailscale client (see /client/README.md), dressed as
// the Go one, so the app can run on either. Open the app with ?client=rust
// to use it.
//
// The Go client is a state machine the app watches: it reports a state
// ("NeedsLogin", "Running", ...) and the app reacts. The Rust client is a set
// of calls that resolve when a step is done. This file turns one into the
// other.

const STATE_ID = "rust";  // stored by the app as "ipn-state-rust"

// How long to wait for other devices to be able to answer before letting
// the user browse anyway. The client keeps trying in the background.
const READY_WAIT_MS = 8000;

// What the client has reported, newest last, for the app's "Details" panel.
// The Rust code writes to the console; this keeps a copy.
const log = [];
function note(...parts) {
  const seconds = (performance.now() / 1000).toFixed(1).padStart(6);
  log.push(`${seconds}s  ${parts.map(String).join(" ").trim()}`);
  if (log.length > 300) log.shift();
}
for (const level of ["debug", "log", "info", "warn", "error"]) {
  const original = console[level].bind(console);
  console[level] = (...args) => { note(...args); original(...args); };
}

export async function createIPN(config) {
  note("loading the Rust client");
  globalThis.__portholeClientCrashed = (message) => {
    note("the client crashed:", message);
    config.panicHandler?.(message);
  };
  const { default: init, Client } = await import("/client/porthole_client.js");
  await init();
  config.onProgress?.(1);

  const saved = config.stateStorage.getState(STATE_ID) || undefined;
  const client = new Client(config.hostname, config.ephemeral !== false, saved);
  // The keys are new unless they were saved before; either way keep them, so
  // a reload comes back as the same device.
  config.stateStorage.setState(STATE_ID, client.state());

  let callbacks = null;
  let signInAddress = null;   // where the user signs in, once known
  let loginWanted = false;    // the app asked for the address
  let stopped = false;

  const state = (name) => {
    if (stopped) return;
    note("state:", name);
    callbacks?.notifyState(name);
  };
  const offerSignIn = () => {
    if (loginWanted && signInAddress) callbacks.notifyBrowseToURL(signInAddress);
  };

  async function start() {
    state("Starting");
    note("connecting to the control server");
    const address = await client.signInAddress();
    note(address === undefined || address === null ? "already signed in" : address ? "sign-in needed" : "waiting for an admin's approval");
    if (address !== undefined && address !== null) {
      if (address) {
        signInAddress = address;
        state("NeedsLogin");
        offerSignIn();
      } else {
        state("NeedsMachineAuth");
      }
      await client.waitForSignIn(() => state("NeedsMachineAuth"));
      signInAddress = null;
      note("signed in");
      state("Starting");
    }

    let running = false;
    note("asking for the device list");
    await client.watch((devices) => {
      if (stopped) return;
      callbacks.notifyNetMap(devices);
      // "Running" waits for the first device list and, for a while, for
      // other devices to know where to answer us.
      if (!running) {
        running = true;
        note("device list received; waiting for a home relay");
        const waited = new Promise((resolve) => setTimeout(() => resolve("late"), READY_WAIT_MS));
        Promise.race([client.ready().then(() => "ready"), waited]).then((how) => {
          note(how === "ready" ? "other devices can answer" : "still no home relay; continuing, sites may not load yet");
          state("Running");
        });
      }
    }, () => { note("the control server stopped sending updates"); state("Stopped"); });
    note("device list requested");
  }

  return {
    run(given) {
      callbacks = given;
      start().catch((error) => {
        note("stopped:", error?.message ?? error);
        config.panicHandler?.(String(error?.message ?? error));
      });
    },
    // Everything the client has reported so far, as text.
    diagnostics: () => log.join("\n"),
    login() {
      loginWanted = true;
      offerSignIn();
    },
    logout() {
      // The app waits for "NeedsLogin", then deletes the keys and reloads.
      let finished = false;
      const done = (how) => {
        if (finished) return;
        finished = true;
        note("sign-out:", how);
        state("NeedsLogin");
        stopped = true;
      };
      client.signOut().then(() => done("done"), (error) => done("did not reach the control server: " + (error?.message ?? error)));
      // The keys are deleted either way, so do not keep the user waiting.
      setTimeout(() => done("no answer yet; continuing"), 2500);
    },
    request: (options) => client.request(options),
    websocket: (options) => client.websocket(options),
    cookies: (url) => client.cookies(url),
    setCookie: (url, line) => client.setCookie(url, line),
    exportCookies: () => client.exportCookies(),
    importCookies: (json) => client.importCookies(json),
  };
}
