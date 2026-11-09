import type { LoginStart } from "../api/types.ts";

export const DEFAULT_HELPER = "http://127.0.0.1:8317";
export const isLoopback = (hostname: string) => hostname === "localhost" || hostname === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(hostname);

export function helperOrigin(raw: string): string {
  const url = new URL(raw);
  if (url.protocol !== "http:" || !isLoopback(url.hostname) || url.username || url.password || url.pathname !== "/" || url.search || url.hash) {
    throw new Error("The sign-in helper must use an http:// loopback address on this device.");
  }
  return url.origin;
}

/** Open synchronously from the click, before any awaited API call can block a popup. */
export async function browserLogin(start: () => Promise<LoginStart>, helper: string): Promise<void> {
  const origin = helperOrigin(helper);
  const popup = window.open(`${origin}/sap/oauth/start#${encodeURIComponent(location.origin)}`, "_blank");
  if (!popup) throw new Error("Allow popups for this dashboard, then start sign-in again.");
  await new Promise<void>((resolve, reject) => {
    let ready = false;
    const finish = (problem?: Error) => {
      clearTimeout(timer);
      clearInterval(closed);
      window.removeEventListener("message", receive);
      if (problem) { popup.close(); reject(problem); } else resolve();
    };
    const receive = (event: MessageEvent) => {
      if (event.source !== popup || event.origin !== origin) return;
      if (event.data?.type === "sap-oauth-ready" && !ready) {
        ready = true;
        void start().then(login => {
          if (!login.callback_relay) throw new Error("Update this dashboard's proxy to support automatic callback helpers.");
          popup.postMessage({type: "sap-oauth-start", url: login.url}, origin);
        }).catch(error => finish(error));
      } else if (ready && event.data?.type === "sap-oauth-registered") finish();
      else if (event.data?.type === "sap-oauth-error") finish(new Error(event.data.message));
    };
    const timer = setTimeout(() => finish(new Error(ready ? "Sign-in setup timed out. Try again." : `Start sap-router on this device at ${origin}, then try again.`)), 20000);
    const closed = setInterval(() => { if (popup.closed) finish(new Error("Sign-in was closed.")); }, 500);
    window.addEventListener("message", receive);
  });
}
