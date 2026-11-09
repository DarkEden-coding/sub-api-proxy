import assert from "node:assert/strict";
import test from "node:test";
import { browserLogin, helperOrigin } from "./oauth.ts";

test("helper addresses stay on this browser's device", () => {
  assert.equal(helperOrigin("http://localhost:8317/"), "http://localhost:8317");
  assert.equal(helperOrigin("http://[::1]:8317"), "http://[::1]:8317");
  for (const address of ["https://localhost:8317", "http://100.64.0.1:8317", "http://localhost.evil.test", "http://user:secret@localhost", "http://localhost/path", "http://localhost?key=secret"]) {
    assert.throws(() => helperOrigin(address));
  }
});

test("remote sign-in waits for the helper and accepts messages only from that window and origin", async () => {
  const listeners = new Set<(event: MessageEvent) => void>();
  const location = { hostname: "proxy.example.test", origin: "https://proxy.example.test" };
  let requested = 0;
  let opened = "";
  let payload: unknown;
  const popup = {
    closed: false,
    close() { this.closed = true; },
    postMessage(value: unknown, origin: string) {
      assert.equal(origin, "http://127.0.0.1:8317");
      payload = value;
      emit(popup, origin, "sap-oauth-registered");
    },
  };
  const emit = (source: unknown, origin: string, type: string) => {
    for (const listener of listeners) listener({ source, origin, data: { type } } as MessageEvent);
  };
  const fakeWindow = {
    open(url: string) { opened = url; return popup; },
    addEventListener(_type: string, handler: (event: MessageEvent) => void) { listeners.add(handler); },
    removeEventListener(_type: string, handler: (event: MessageEvent) => void) { listeners.delete(handler); },
  };
  const oldWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const oldLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  Object.defineProperty(globalThis, "window", { value: fakeWindow, configurable: true });
  Object.defineProperty(globalThis, "location", { value: location, configurable: true });
  try {
    const login = browserLogin(async () => { requested++; return { url: "https://auth.openai.com/oauth/authorize?state=fake", login_id: "fake", callback_relay: true }; }, "http://127.0.0.1:8317");
    assert.equal(opened, "http://127.0.0.1:8317/sap/oauth/start#https%3A%2F%2Fproxy.example.test");
    assert.equal(requested, 0);
    emit({}, "http://127.0.0.1:8317", "sap-oauth-ready");
    emit(popup, "https://evil.example.test", "sap-oauth-ready");
    assert.equal(requested, 0);
    emit(popup, "http://127.0.0.1:8317", "sap-oauth-ready");
    await login;
    assert.equal(requested, 1);
    assert.deepEqual(payload, { type: "sap-oauth-start", url: "https://auth.openai.com/oauth/authorize?state=fake" });
    assert.equal(popup.closed, false);
    assert.equal(listeners.size, 0);
  } finally {
    if (oldWindow) Object.defineProperty(globalThis, "window", oldWindow); else Reflect.deleteProperty(globalThis, "window");
    if (oldLocation) Object.defineProperty(globalThis, "location", oldLocation); else Reflect.deleteProperty(globalThis, "location");
  }
});
