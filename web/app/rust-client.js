// The Rust build of the Tailscale client (see /client/README.md), dressed as
// the Go one, so the app can run on either. Open the app with ?client=rust
// to use it.
//
// The Go client is a state machine the app watches: it reports a state
// ("NeedsLogin", "Running", ...) and the app reacts. The Rust client is a set
// of calls that resolve when a step is done. This file turns one into the
// other.

const STATE_ID = "rust";  // stored by the app as "ipn-state-rust"

export async function createIPN(config) {
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

  const state = (name) => { if (!stopped) callbacks?.notifyState(name); };
  const offerSignIn = () => {
    if (loginWanted && signInAddress) callbacks.notifyBrowseToURL(signInAddress);
  };

  async function start() {
    state("Starting");
    const address = await client.signInAddress();
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
      state("Starting");
    }

    let running = false;
    await client.watch((devices) => {
      if (stopped) return;
      callbacks.notifyNetMap(devices);
      // "Running" waits for the first device list and for other devices to
      // know where to answer us.
      if (!running) {
        running = true;
        client.ready().then(() => state("Running"));
      }
    }, () => state("Stopped"));
  }

  return {
    run(given) {
      callbacks = given;
      start().catch((error) => {
        console.debug("[porthole] rust client stopped", error);
        config.panicHandler?.(String(error?.message ?? error));
      });
    },
    login() {
      loginWanted = true;
      offerSignIn();
    },
    logout() {
      // The app waits for "NeedsLogin", then deletes the keys and reloads.
      const done = () => { state("NeedsLogin"); stopped = true; };
      client.signOut().then(done, (error) => {
        console.debug("[porthole] sign-out did not reach the control server", error);
        done();
      });
    },
    request: (options) => client.request(options),
    websocket: (options) => client.websocket(options),
    cookies: (url) => client.cookies(url),
    setCookie: (url, line) => client.setCookie(url, line),
    exportCookies: () => client.exportCookies(),
    importCookies: (json) => client.importCookies(json),
  };
}
