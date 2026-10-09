import init, { Engine } from "./portfolio_v2_browser.js?v=__V2_BUILD__";
import { AssetCache } from "./assets.js?v=__V2_BUILD__";

let engine,
  manifest,
  wanted = [],
  prefetch = [],
  timer,
  awaiting = false,
  dirty = true,
  last = 0;
let active = 0,
  hidden = false,
  reduced = false,
  initialized = false,
  assetsReady = false;
const pending = new Set(),
  failed = new Map(),
  warmed = new Set();
const cache = new AssetCache();
let section = "home",
  sessionId,
  sessionValue,
  sessionPromise,
  submitting = false,
  pendingSubmit;
let socket, reconnectTimer;
let searchTimer, lastSearch;
let editVersion = 0;
function search() {
  const query = engine.search_query();
  if (query === undefined || query === null || query === lastSearch) return;
  lastSearch = query;
  clearTimeout(searchTimer);
  if (query.length) {
    engine.search_results(query, engine.local_search(query));
    schedule();
  }
  if (query.length < 3) return;
  searchTimer = setTimeout(async () => {
    try {
      const response = await fetch(
        `/api/v2/geocode?q=${encodeURIComponent(query)}`,
        { signal: AbortSignal.timeout(15000) },
      );
      if (!response.ok) return;
      const remote = await response.json(),
        local = JSON.parse(engine.local_search(query));
      engine.search_results(
        query,
        JSON.stringify([...local, ...remote].slice(0, 10)),
      );
      schedule();
    } catch {}
  }, 350);
}
async function sessionHttp(path, body) {
  const response = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    cache: "no-store",
    signal: AbortSignal.timeout(15000),
  });
  if (!response.ok)
    throw new Error(`Session request failed (${response.status})`);
  const snapshot = await response.json();
  if (engine.session(JSON.stringify(snapshot))) schedule();
  sessionId = snapshot.session_id;
  sessionValue = snapshot;
  postMessage({ type: "session-id", id: sessionId });
  return snapshot;
}
async function ensureSession() {
  if (sessionPromise) return sessionPromise;
  if (sessionValue) return sessionValue;
  sessionPromise = (async () => {
    if (sessionId) {
      try {
        return await sessionHttp(`/api/v2/sessions/${sessionId}`);
      } catch (error) {
        throw new Error(
          "Saved conversation unavailable; retry after reconnecting.",
        );
      }
    }
    return sessionHttp("/api/v2/sessions", {});
  })();
  try {
    return await sessionPromise;
  } finally {
    sessionPromise = undefined;
  }
}
async function pollSession() {
  if (!sessionId || hidden) return;
  if (socket && socket.readyState < 2) return;
  clearTimeout(reconnectTimer);
  const url = new URL("/api/v2/session", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("session", sessionId);
  socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  socket.onopen = () => {
    engine.ask_status("");
    schedule();
  };
  socket.onmessage = ({ data }) => {
    try {
      if (!(data instanceof ArrayBuffer) || data.byteLength > 3 * 1024 * 1024)
        throw new Error("Invalid session frame");
      if (engine.session_cbor(new Uint8Array(data))) schedule();
      sessionValue = JSON.parse(engine.session_snapshot());
      if (
        pendingSubmit &&
        sessionValue?.exchanges.some(
          (e) => e.request_id === pendingSubmit.request_id,
        )
      )
        pendingSubmit = undefined;
    } catch (error) {
      engine.ask_status("Session protocol error; update the client.");
      schedule();
      socket.close();
    }
  };
  socket.onclose = () => {
    if (!hidden) {
      engine.ask_status("Disconnected; conversation will resume on reconnect.");
      schedule();
      reconnectTimer = setTimeout(pollSession, 2000);
    }
  };
  socket.onerror = () => socket.close();
}
async function submitQuestion() {
  if (submitting) return;
  const question = engine.question();
  if (!question.trim()) return;
  submitting = true;
  try {
    await ensureSession();
    pendingSubmit ||= {
      request_id: crypto.randomUUID().replaceAll("-", ""),
      question,
    };
    engine.mark_submitted(pendingSubmit.request_id);
    engine.ask_status("sending…");
    schedule();
    await sessionHttp(`/api/v2/sessions/${sessionId}/requests`, pendingSubmit);
    pendingSubmit = undefined;
    pollSession();
  } catch (error) {
    engine.ask_status(`${error.message}; Enter retries the same request.`);
    schedule();
  } finally {
    submitting = false;
  }
}
async function cancelQuestion() {
  const running = sessionValue?.exchanges.find((e) => e.status === "running");
  if (!running) return;
  engine.ask_status("cancelling…");
  schedule();
  try {
    await sessionHttp(
      `/api/v2/sessions/${sessionId}/requests/${running.request_id}/cancel`,
      {},
    );
  } catch (error) {
    engine.ask_status("Cancellation pending; retry after reconnecting.");
    schedule();
  }
}
async function newConversation() {
  if (submitting) return;
  if (sessionPromise) {
    try {
      await sessionPromise;
    } catch {}
  }
  clearTimeout(reconnectTimer);
  if (socket) {
    socket.onclose = null;
    socket.close();
    socket = undefined;
  }
  sessionId = undefined;
  sessionValue = undefined;
  pendingSubmit = undefined;
  engine.reset_session();
  schedule();
  try {
    await ensureSession();
    pollSession();
  } catch (error) {
    engine.ask_status(error.message);
    schedule();
  }
}
const id = (tile) => tile.join("/");
const status = (message) => postMessage({ type: "status", message });

function schedule() {
  dirty = true;
  if (!timer && !awaiting && !hidden && engine && initialized)
    timer = setTimeout(present, 0);
}
function present() {
  timer = undefined;
  if (hidden || awaiting) return;
  const now = performance.now();
  engine.tick(last ? Math.min((now - last) / 1000, 10) : 0);
  last = now;
  const started = performance.now();
  const cells = engine.frame();
  section = engine.section();
  const meta = JSON.parse(engine.metadata());
  meta.editVersion = editVersion;
  meta.pendingSubmit = pendingSubmit;
  meta.variant.reduced_motion = reduced;
  wanted = JSON.parse(engine.demand());
  awaiting = true;
  dirty = false;
  postMessage(
    {
      type: "frame",
      cells: cells.buffer,
      meta,
      buildMs: performance.now() - started,
    },
    [cells.buffer],
  );
  pump();
}

function pump() {
  if (!manifest?.available || !assetsReady || hidden) return;
  // Foreground demands always precede speculative route warming. Two requests
  // bound network and decoder pressure, while camera changes replace the queue.
  while (active < 2) {
    const tile = [
      ...JSON.parse(engine.preview_demand()),
      ...wanted,
      ...prefetch.filter((tile) => !warmed.has(id(tile))),
    ].find(
      (tile) =>
        (!engine.has_tile(...tile) ||
          (manifest.terrain_format === "tmhg-tiles-v1" &&
            !engine.has_terrain_tile(...tile))) &&
        !pending.has(id(tile)) &&
        (failed.get(id(tile)) || 0) < Date.now(),
    );
    if (!tile) return;
    const key = id(tile);
    pending.add(key);
    active++;
    (async () => {
      if (!engine.has_tile(...tile)) {
        const bytes = await cache.get(
          `${manifest.base}/tiles/${key}`,
          8 * 1024 * 1024,
        );
        if (!engine.tile(...tile, bytes))
          throw new Error(
            "invalid vector data or decoded cache budget exceeded",
          );
      }
      if (
        manifest.terrain_format === "tmhg-tiles-v1" &&
        !engine.has_terrain_tile(...tile)
      ) {
        const bytes = await cache.get(
          `${manifest.base}/terrain/${key}`,
          64 * 1024,
        );
        if (!engine.terrain_tile(...tile, bytes))
          throw new Error(
            "invalid elevation data or terrain cache budget exceeded",
          );
      }
    })()
      .then(() => {
        failed.delete(key);
        warmed.add(key);
        const preview = JSON.parse(engine.preview_demand());
        if ([...wanted, ...preview].some((t) => id(t) === key)) {
          status("");
          // Partial arrivals cannot be installed atomically. Avoid composing
          // identical full frames for every tile completion.
          if ([wanted, preview].some((tiles) => tiles.length && tiles.every(
            (t) => engine.has_tile(...t) &&
              (manifest.terrain_format !== "tmhg-tiles-v1" || engine.has_terrain_tile(...t)),
          ))) schedule();
        }
      })
      .catch((error) => {
        failed.set(key, Date.now() + 15000);
        if (wanted.some((t) => id(t) === key))
          status(
            `Map data delayed: ${error.message}. Retaining the last complete view.`,
          );
        setTimeout(pump, 15050);
      })
      .finally(() => {
        active--;
        pending.delete(key);
        pump();
      });
  }
}

onmessage = async ({ data }) => {
  try {
    if (data.type === "init") {
      editVersion = data.editVersion || 0;
      await init({
        module_or_path: new URL(
          "./portfolio_v2_browser_bg.wasm?v=__V2_BUILD__",
          import.meta.url,
        ),
      });
      engine = new Engine();
      engine.resize(data.width, data.height, data.scale);
      reduced = data.reduced;
      engine.reduced_motion(reduced);
      engine.bootstrap(await cache.get("/api/v2/bootstrap", 256 * 1024, false));
      if (data.restore) {
        engine.restore_local(JSON.stringify(data.restore));
        section = engine.section();
      }
      if (
        data.pendingSubmit &&
        /^[a-f0-9]{32}$/.test(data.pendingSubmit.request_id) &&
        typeof data.pendingSubmit.question === "string" &&
        data.pendingSubmit.question.length <= 4096
      ) {
        pendingSubmit = data.pendingSubmit;
        engine.mark_submitted(pendingSubmit.request_id);
      }
      if (data.appearance)
        engine.restore_appearance(
          data.appearance.theme || "dark",
          data.appearance.package || "canonical",
          data.appearance.color || "color",
        );
      initialized = true;
      if (/^[a-f0-9]{32}$/.test(data.sessionId || ""))
        sessionId = data.sessionId;
      schedule();
      const response = await fetch("/api/v2/map", { cache: "no-cache" });
      if (!response.ok)
        throw new Error(`Map catalogue HTTP ${response.status}`);
      manifest = await response.json();
      if (
        manifest.protocol !== 2 ||
        !/^\/map\/v2\/[a-f0-9]{64}$/.test(manifest.base)
      )
        throw new Error("Unsupported map catalogue");
      if (!manifest.available) {
        status("The backend has no map archive mounted.");
        return;
      }
      // Mandatory base resources load before installing a vector generation.
      engine.overlay(
        await cache.get(`${manifest.base}/states.tmap`, 2 * 1024 * 1024),
      );
      if (manifest.terrain_format === "tmhg-tiles-v1") {
        engine.tiled_terrain(true);
      } else {
        engine.terrain(
          await cache.get(`${manifest.base}/terrain.tmhg`, 64 * 1024 * 1024),
        );
      }
      assetsReady = true;
      prefetch = JSON.parse(engine.prefetch());
      schedule();
      pump();
      return;
    }
    if (!engine) return;
    if (data.type === "ack") {
      awaiting = false;
      if (dirty || engine.animating()) {
        if (!timer && !hidden)
          timer = setTimeout(present, engine.animating() ? 16 : 0);
      } else last = 0;
      return;
    }
    if (data.type === "visibility") {
      hidden = data.hidden;
      last = 0;
      if (hidden) {
        clearTimeout(timer);
        timer = undefined;
        clearTimeout(reconnectTimer);
        socket?.close();
      } else {
        schedule();
        pump();
        if (sessionId) pollSession();
      }
      return;
    }
    let redraw = false;
    for (const action of data.actions || [data]) {
      if (action.type !== "pointer") redraw = true;
      switch (action.type) {
        case "canonical-fallback":
          engine.canonical_fallback();
          break;
        case "appearance":
          engine.appearance(action.id);
          break;
        case "key":
          if (section === "ask" && action.key === "Enter") submitQuestion();
          else {
            engine.key(action.key);
            const nav = {
              1: "home",
              2: "experience",
              3: "projects",
              4: "skills",
              5: "taste",
              6: "ask",
              Escape: "home",
            };
            if (
              nav[action.key] &&
              (section !== "ask" || action.key === "Escape")
            )
              section = nav[action.key];
          }
          section = engine.section();
          search();
          if (section === "ask")
            ensureSession()
              .then(() => pollSession())
              .catch((error) => {
                engine.ask_status(error.message);
                schedule();
              });
          break;
        case "navigate":
          engine.navigate(action.id);
          section = engine.section();
          if (section === "ask")
            ensureSession()
              .then(() => pollSession())
              .catch((error) => {
                engine.ask_status(error.message);
                schedule();
              });
          break;
        case "resize":
          engine.resize(action.width, action.height, action.scale);
          break;
        case "drag":
          engine.drag(action.x, action.y, action.dx, action.dy);
          engine.pointer(action.x, action.y, action.dx, action.dy, true);
          break;
        case "zoom":
          engine.zoom(action.x, action.y, action.delta);
          break;
        case "motion":
          reduced = action.reduced;
          engine.reduced_motion(reduced);
          break;
        case "pointer":
          redraw = engine.pointer(action.x, action.y, 0, 0, false) || redraw;
          break;
        case "scroll":
          engine.scroll(action.delta);
          break;
        case "release":
          engine.release_pointer();
          break;
        case "edit-question":
          editVersion = action.version || editVersion;
          engine.edit_question(action.text);
          break;
        case "submit":
          submitQuestion();
          break;
        case "cancel":
          cancelQuestion();
          break;
        case "new-session":
          newConversation();
          break;
      }
    }
    if (redraw) schedule();
  } catch (error) {
    status(error.message || String(error));
  }
};
