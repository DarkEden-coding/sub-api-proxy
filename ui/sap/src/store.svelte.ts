import { mergeEvents } from "./model/events.ts";
import { fetchFlow, isDemo, managementKey, RequestError, subscribe, useDemo } from "./api/client.ts";
import type { AccountView, EventView, FlowSnapshot, ThreadKey, ThreadView } from "./api/types.ts";

export type Status = "connecting" | "live" | "demo" | "offline" | "unauthorized";
export const PAGES = ["flow", "accounts", "threads", "nodes", "events", "settings"] as const;
export type Page = (typeof PAGES)[number];

const pageFromHash = (): Page => {
  const name = location.hash.replace(/^#\/?/, "");
  return (PAGES as readonly string[]).includes(name) ? (name as Page) : "flow";
};

class Store {
  snapshot = $state<FlowSnapshot | null>(null);
  /** Recent events, newest first: the snapshot's, then each streamed `event`, capped at 500. */
  events = $state<EventView[]>([]);
  status = $state<Status>("connecting");
  error = $state("");
  page = $state<Page>(pageFromHash());
  selected = $state<ThreadKey | null>(null);
  /** Collapsed thread groups, by client id or `client|machine`. Everything starts open. */
  collapsed = $state<Record<string, boolean>>({});
  #stop: (() => void) | null = null;

  /** The selected thread, else the first streaming one, else the first. */
  selectedThread = $derived.by((): ThreadView | null => {
    const threads = this.snapshot?.threads ?? [];
    return threads.find((t) => t.key === this.selected) ?? threads.find((t) => t.streaming) ?? threads[0] ?? null;
  });

  accountById = $derived(new Map((this.snapshot?.accounts ?? []).map((a) => [a.id, a])));

  accountLabel(id: string | null): string {
    return id ? (this.accountById.get(id)?.label ?? id) : "unassigned";
  }

  accountsOf(provider: AccountView["provider"]): AccountView[] {
    return (this.snapshot?.accounts ?? []).filter((a) => a.provider === provider);
  }

  start() {
    this.#stop?.();
    if (!isDemo() && !managementKey()) {
      this.status = "unauthorized";
      this.error = "Enter the management key in Settings.";
      this.go("settings");
      return;
    }
    this.status = isDemo() ? "demo" : "connecting";
    this.#stop = subscribe({
      onOpen: () => (this.status = isDemo() ? "demo" : "live"),
      onSnapshot: (s) => {
        this.snapshot = s;
        this.addEvents(s.events);
        this.error = "";
      },
      onEvent: (e) => this.addEvents([e]),
      onError: (e) => this.#failed(e),
    });
    if (!isDemo()) void fetchFlow().then((s) => (this.snapshot ??= s)).catch(() => {});
  }

  addEvents(incoming: EventView[]) {
    this.events = mergeEvents(this.events, incoming);
  }

  #failed(e: RequestError) {
    this.error = e.message;
    if (e.status === 401 || e.status === 403) {
      this.status = "unauthorized";
    } else if (import.meta.env.DEV && !this.snapshot) {
      useDemo();
      this.start();
    } else {
      this.status = "offline";
    }
  }

  toggle(id: string) {
    this.collapsed[id] = !this.collapsed[id];
  }

  go(page: Page) {
    location.hash = `#/${page}`;
  }

  listenForHash() {
    addEventListener("hashchange", () => (this.page = pageFromHash()));
  }
}

export const store = new Store();
