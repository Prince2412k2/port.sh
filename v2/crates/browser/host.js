const stage = document.getElementById("stage");
const editor = document.getElementById("ask-editor"),
  questionInput = document.getElementById("question-input");
let editVersion = 0;
editor.onsubmit = (event) => {
  event.preventDefault();
  send({ type: "submit" });
};
document.getElementById("new-conversation").onclick = () =>
  send({ type: "new-session" });
document.getElementById("cancel-answer").onclick = () =>
  send({ type: "cancel" });
questionInput.oninput = () =>
  send({
    type: "edit-question",
    text: questionInput.value,
    version: ++editVersion,
  });
questionInput.onkeydown = (event) => {
  if (event.isComposing) return;
  if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    send({ type: "submit" });
  }
  if (event.key === "Escape") {
    event.preventDefault();
    send({ type: "navigate", id: "home" });
    questionInput.blur();
  }
};
let worker = new Worker(
  new URL("./worker.js?v=__V2_BUILD__", import.meta.url),
  {
    type: "module",
    name: "portfolio-engine",
  },
);
const motion = matchMedia("(prefers-reduced-motion: reduce)");
const metrics = (window.portfolioV2Worker = {
  frames: 0,
  buildMs: 0,
  decodedBytes: 0,
  mainMs: 0,
});
let meta,
  semanticKey = "",
  pending = [],
  inputFrame = 0,
  pointer,
  dragged = false;
let componentStatus = "",
  appearanceKey = "";
let lastLocal,
  lastPending,
  workerRestarts = 0,
  recovering = false;
let previousWords, previousCells;
function viewport() {
  return { width: innerWidth, height: innerHeight, scale: devicePixelRatio };
}
function send(action) {
  // Collapse high-rate input into one worker message per display opportunity.
  const previous = pending[pending.length - 1];
  if (action.type === "resize" && previous?.type === "resize") pending.pop();
  if (action.type === "drag" && previous?.type === "drag") {
    previous.dx += action.dx;
    previous.dy += action.dy;
  } else pending.push(action);
  if (!inputFrame)
    inputFrame = requestAnimationFrame(() => {
      inputFrame = 0;
      worker.postMessage({ actions: pending });
      pending = [];
    });
}
function status(message) {
  const element = document.getElementById("status");
  element.textContent = message;
  element.dataset.empty = String(!message);
}
function node(tag, text, parent) {
  const element = document.createElement(tag);
  element.textContent = text;
  parent.append(element);
  return element;
}
function semantics(next) {
  const key = JSON.stringify([
    next.profile,
    next.section,
    next.mapDescription,
    next.content,
  ]);
  if (key === semanticKey || !next.profile) return;
  semanticKey = key;
  const root = document.getElementById("semantic");
  const article = document.createElement("article");
  const nav = node("nav", "", article);
  nav.setAttribute("aria-label", "Sections");
  for (const id of [
    "home",
    "experience",
    "projects",
    "skills",
    "taste",
    "ask",
  ]) {
    const button = node("button", id, nav);
    button.type = "button";
    button.setAttribute("aria-current", String(next.section === id));
    button.onclick = () => send({ type: "navigate", id });
  }
  const p = next.profile;
  node("h1", p.name, article);
  node("p", `${p.role} · ${p.location}`, article);
  if (next.section === "home") {
    node("p", p.pitch, article);
    node("h2", "Now", article);
    node("p", p.now, article);
    const list = node("ul", "", article);
    for (const contact of p.contacts) {
      const item = node("li", "", list);
      const link = node("a", `${contact.label}: ${contact.value}`, item);
      if (/^(https:\/\/|mailto:|ssh:\/\/)/.test(contact.href))
        link.href = contact.href;
    }
  } else if (next.section === "experience") {
    node("h2", "Experience map", article);
    node("p", next.mapDescription, article);
    node(
      "p",
      "Explore the route from school in Kapadwanj to university and engineering work in Ahmedabad. Arrow keys pan, plus/minus zoom, n/b move between stops, and Enter replays a stop.",
      article,
    );
    for (const [label, key] of [
      ["Previous stop", "b"],
      ["Next stop", "n"],
      ["Zoom in", "+"],
      ["Zoom out", "-"],
    ]) {
      node("button", label, article).onclick = () => send({ type: "key", key });
    }
  } else if (next.section === "projects" && next.content) {
    const project = next.content;
    node("h2", project.name, article);
    node("p", project.tag, article);
    node("p", project.stats, article);
    node("p", project.tools.join(", "), article);
    for (const beat of project.beats) {
      node("h3", beat.head, article);
      node("p", beat.body, article);
    }
    for (const [label, key] of [
      ["Previous project", "ArrowLeft"],
      ["Next project", "ArrowRight"],
    ])
      node("button", label, article).onclick = () => send({ type: "key", key });
  } else if (next.section === "skills") {
    node("h2", "Skills", article);
    node("p", next.content.skills.join(", "), article);
  } else if (next.section === "taste") {
    const sheet = next.content.sheet;
    node("h2", "Taste", article);
    node("p", sheet.open, article);
    for (const entry of [...sheet.figures, ...sheet.works, ...sheet.threads]) {
      node("h3", entry.name, article);
      node("p", entry.from, article);
      if (entry.quote) node("blockquote", entry.quote, article);
      node("p", entry.body, article);
    }
    node("p", sheet.close, article);
  } else if (next.section === "ask") {
    node("h2", "Ask", article);
    const transcript = node("div", "", article);
    transcript.setAttribute("role", "log");
    transcript.setAttribute("aria-live", "polite");
    for (const exchange of next.content.exchanges) {
      node("h3", exchange.question, transcript);
      node("p", exchange.answer, transcript);
      if (exchange.error) node("p", exchange.error, transcript);
      (exchange.presentations || []).forEach((panel, index) => {
        node(
          "button",
          `Show ${panel.kind}: ${panel.title || panel.id}`,
          transcript,
        ).onclick = () =>
          send({
            type: "navigate",
            id: `panel:${exchange.request_id}:${index}`,
          });
      });
    }
    if (next.content.diagram) {
      const d = next.content.diagram;
      node("h3", d.title, article);
      for (const n of d.nodes) node("p", `${n.id}: ${n.label}`, article);
      for (const e of d.edges)
        node("p", `${e.from} → ${e.to}: ${e.label}`, article);
    }
    const cancel = node("button", "Cancel answer", article);
    cancel.onclick = () => send({ type: "cancel" });
    node("p", next.content.status, article);
  }
  root.replaceChildren(article);
}
function unpack(buffer, metadata) {
  const view = new DataView(buffer),
    raw = new Uint8Array(buffer);
  if (buffer.byteLength !== metadata.cols * metadata.rows * 24)
    throw new Error("Invalid worker frame");
  const cells = new Array(metadata.cols * metadata.rows);
  const words = new Uint32Array(buffer);
  for (let i = 0; i < cells.length; i++) {
    const offset = i * 24;
    const word = i * 6;
    if (
      previousWords?.length === words.length &&
      words[word] === previousWords[word] &&
      words[word + 1] === previousWords[word + 1] &&
      words[word + 2] === previousWords[word + 2] &&
      words[word + 3] === previousWords[word + 3] &&
      words[word + 4] === previousWords[word + 4] &&
      words[word + 5] === previousWords[word + 5]
    ) {
      cells[i] = previousCells[i];
      continue;
    }
    cells[i] = {
      glyph: String.fromCodePoint(view.getUint32(offset, true)),
      foreground: [
        raw[offset + 4],
        raw[offset + 5],
        raw[offset + 6],
        raw[offset + 7],
      ],
      background: [
        raw[offset + 8],
        raw[offset + 9],
        raw[offset + 10],
        raw[offset + 11],
      ],
      detail: view.getUint16(offset + 12, true),
      bold: !!raw[offset + 14],
      material: view.getUint16(offset + 16, true),
      layer: view.getUint16(offset + 18, true),
      depth: view.getUint16(offset + 20, true),
    };
  }
  previousWords = words;
  previousCells = cells;
  return {
    fallback: { cols: metadata.cols, rows: metadata.rows, cells },
    details: metadata.details,
    variant: metadata.variant,
  };
}
worker.onmessage = ({ data, target: sourceWorker }) => {
  if (sourceWorker !== worker) return;
  if (data.type === "session-id") {
    try {
      localStorage.setItem("portfolio-v2-session", data.id);
    } catch {}
    return;
  }
  if (data.type === "status") {
    componentStatus = data.message;
    status(data.message);
    return;
  }
  if (data.type !== "frame") return;
  requestAnimationFrame(() => {
    if (sourceWorker !== worker) return;
    const started = performance.now();
    try {
      meta = data.meta;
      if (recovering && meta.section === "experience" && meta.loading) return;
      recovering = false;
      lastLocal = meta.local;
      lastPending = meta.pendingSubmit;
      editor.hidden = meta.section !== "ask";
      if (meta.section === "ask") {
        const cw = innerWidth / meta.cols, ch = innerHeight / meta.rows;
        editor.style.left = `${(meta.cols >= 90 ? 10 : 3) * cw}px`;
        editor.style.top = `${(meta.rows - 4) * ch}px`;
        editor.style.width = `${Math.min(meta.cols - (meta.cols >= 90 ? 13 : 6), 100) * cw}px`;
        if (
          (meta.editVersion || 0) >= editVersion &&
          questionInput.value !== meta.content.question
        )
          questionInput.value = meta.content.question;
      }
      const frame = unpack(data.cells, meta);
      const preference = JSON.stringify({
        theme: meta.theme.toLowerCase(),
        package: meta.variant.package,
        color: meta.variant.color,
      });
      if (preference !== appearanceKey) {
        appearanceKey = preference;
        try {
          localStorage.setItem("portfolio-v2-appearance", preference);
        } catch {}
      }
      window.portfolioV2.render(frame);
      semantics(meta);
      document.documentElement.dataset.theme = meta.theme.toLowerCase();
      const packages = ["canonical", "crt", "vhs"];
      const index = packages.indexOf(meta.variant.package);
      document.getElementById("package-name").textContent =
        meta.variant.package.toUpperCase();
      document
        .getElementById("package-toggle")
        .setAttribute(
          "aria-label",
          `Render package: ${meta.variant.package}; activate for ${packages[(index + 1) % packages.length]}`,
        );
      document
        .getElementById("theme-toggle")
        .setAttribute("aria-label", `Theme: ${meta.theme}; activate to switch`);
      window.portfolioV2Controls.paint(meta.theme.toLowerCase(), index);
      status(meta.loading ? componentStatus || "Loading map detail…" : "");
      metrics.frames++;
      metrics.buildMs = data.buildMs;
      metrics.decodedBytes = meta.decodedBytes;
      metrics.mainMs = performance.now() - started;
    } finally {
      worker.postMessage({ type: "ack" });
    }
  });
};
worker.onerror = (event) => {
  if (workerRestarts++ < 2) {
    const onmessage = worker.onmessage,
      onerror = worker.onerror;
    worker.terminate();
    worker = new Worker(
      new URL("./worker.js?v=__V2_BUILD__", import.meta.url),
      { type: "module", name: "portfolio-engine" },
    );
    worker.onmessage = onmessage;
    worker.onerror = onerror;
    recovering = true;
    let sessionId;
    try {
      sessionId = localStorage.getItem("portfolio-v2-session");
    } catch {}
    const restore = lastLocal
      ? {
          ...lastLocal,
          question:
            meta?.section === "ask" ? questionInput.value : lastLocal.question,
        }
      : undefined;
    worker.postMessage({
      type: "init",
      ...viewport(),
      reduced: motion.matches,
      restore,
      pendingSubmit: lastPending,
      sessionId,
      editVersion,
      appearance: meta
        ? {
            theme: meta.theme,
            package: meta.variant.package,
            color: meta.variant.color,
          }
        : undefined,
    });
    status("Recovering the client worker…");
    return;
  }
  status(`Client worker failed: ${event.message}. Reload to reconnect.`);
  document.documentElement.dataset.renderer = "semantic-fallback";
  document.getElementById("semantic").classList.add("visible-fallback");
};
let savedSession, savedAppearance;
try {
  savedSession = localStorage.getItem("portfolio-v2-session");
  savedAppearance = JSON.parse(
    localStorage.getItem("portfolio-v2-appearance") || "null",
  );
} catch {}
worker.postMessage({
  type: "init",
  ...viewport(),
  reduced: motion.matches,
  sessionId: savedSession,
  appearance: savedAppearance,
});
addEventListener("resize", () => send({ type: "resize", ...viewport() }));
motion.addEventListener("change", () =>
  send({ type: "motion", reduced: motion.matches }),
);
document.addEventListener("visibilitychange", () =>
  worker.postMessage({ type: "visibility", hidden: document.hidden }),
);
document.getElementById("theme-toggle").onclick = () =>
  send({ type: "appearance", id: "theme" });
document.getElementById("package-toggle").onclick = () =>
  send({ type: "appearance", id: "package" });
addEventListener("keydown", (event) => {
  if (
    meta?.section === "ask" &&
    event.ctrlKey &&
    event.key === "x" &&
    meta.content.exchanges.some((e) => e.status === "running")
  ) {
    event.preventDefault();
    send({ type: "cancel" });
    return;
  }
  if (
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    /^(INPUT|TEXTAREA|SELECT)$/.test(event.target.tagName)
  )
    return;
  if (event.target.closest?.("button,a") && ["Enter", " "].includes(event.key))
    return;
  if (
    event.key === "Tab" &&
    !(
      event.target === document.body &&
      ["skills", "projects"].includes(meta?.section)
    )
  )
    return;
  if (
    meta?.section === "experience" &&
    meta.search !== null &&
    (event.key.length === 1 ||
      ["Backspace", "Enter", "Escape", "ArrowUp", "ArrowDown"].includes(
        event.key,
      ))
  ) {
    event.preventDefault();
    send({ type: "key", key: event.key });
    return;
  }
  if (
    meta?.section === "ask" &&
    (event.key.length === 1 ||
      ["Backspace", "Enter", "Escape"].includes(event.key))
  ) {
    event.preventDefault();
    send({ type: "key", key: event.key });
    return;
  }
  if (
    [
      "1",
      "0",
      "Tab",
      " ",
      "2",
      "3",
      "4",
      "5",
      "6",
      "?",
      "/",
      "PageDown",
      "PageUp",
      "Escape",
      "Enter",
      "i",
      "p",
      "c",
      "n",
      "b",
      "h",
      "j",
      "k",
      "l",
      "ArrowLeft",
      "ArrowRight",
      "ArrowUp",
      "ArrowDown",
      "+",
      "=",
      "-",
      "_",
      "u",
      "o",
      ",",
      ".",
      "m",
      "v",
      "t",
      "f",
      "r",
      "[",
      "]",
    ].includes(event.key)
  ) {
    event.preventDefault();
    send({ type: "key", key: event.key });
  }
});
function point(event) {
  const box = stage.getBoundingClientRect();
  return {
    x: ((event.clientX - box.left) / box.width) * (meta?.cols || 1),
    y: ((event.clientY - box.top) / box.height) * (meta?.rows || 1),
  };
}
stage.addEventListener("pointerdown", (event) => {
  if (event.button !== 0 || !["experience", "skills"].includes(meta?.section))
    return;
  pointer = point(event);
  dragged = false;
  stage.setPointerCapture(event.pointerId);
});
stage.addEventListener("pointermove", (event) => {
  if (!pointer) {
    if (["skills", "experience"].includes(meta?.section))
      send({ type: "pointer", ...point(event) });
    return;
  }
  const next = point(event),
    dx = next.x - pointer.x,
    dy = next.y - pointer.y;
  if (dx || dy) {
    send({ type: "drag", ...pointer, dx, dy });
    dragged = true;
    pointer = next;
  }
});
stage.addEventListener("pointerup", () => {
  pointer = undefined;
  send({ type: "release" });
});
stage.addEventListener("pointercancel", () => {
  pointer = undefined;
  send({ type: "release" });
});
stage.addEventListener(
  "wheel",
  (event) => {
    if (meta?.section !== "experience") {
      if (["taste", "projects", "ask"].includes(meta?.section)) {
        event.preventDefault();
        send({ type: "scroll", delta: event.deltaY > 0 ? 3 : -3 });
      }
      return;
    }
    event.preventDefault();
    const scale =
      event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? innerHeight : 1;
    send({
      type: "zoom",
      ...point(event),
      delta: Math.max(-0.5, Math.min(0.5, (-event.deltaY * scale) / 300)),
    });
  },
  { passive: false },
);
stage.addEventListener("click", (event) => {
  if (dragged) {
    dragged = false;
    return;
  }
  const { x, y } = point(event);
  const hit = meta?.hits.find(
    (hit) =>
      x >= hit.x &&
      x < hit.x + hit.width &&
      y >= hit.y &&
      y < hit.y + hit.height,
  );
  if (hit) send({ type: "navigate", id: hit.id });
});
