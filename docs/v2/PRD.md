# PRD: Portfolio V2

**Status:** Draft for approval
**Product:** `sniffkin.tech` / `port.sh`
**Version:** V2
**Last updated:** 2026-08-29
**Supersedes:** `docs/browser-client/PRD.md` after approval

## 1. Summary

Portfolio V2 is a local-rendering, multi-client application built around one deterministic client engine and one structured backend protocol.

The browser, downloadable native terminal client, SSH client, and mosh client share content, behavior, layout rules, map logic, animation timing, themes, and semantic state. The backend sends structured data and authoritative events. It never streams ANSI, cells, screenshots, or animation frames.

The browser uses a GPU renderer designed for the application. Browser render packages are not limited to filters over a completed terminal image. A package may replace scene rasterization, material evaluation, lighting, shadows, terrain presentation, glyph treatment, temporal simulation, and final composition. This permits render modes such as a fully pixel-art map with deterministic palette ramps, terrain normals, building and terrain shadows, animated water, and authored lighting.

The base cell render package remains the parity reference. For the same state, viewport class, clock, content, map data, and theme, browser and terminal clients must produce the same canonical logical cell surface. Advanced browser packages may render that same visual scene differently, but may not change content, behavior, interaction targets, accessibility meaning, or authoritative state.

```text
                           Portfolio backend
                  content, sessions, AI, assets, tools
                                  |
                    structured HTTP + WebSocket API
                                  |
               +------------------+------------------+
               |                  |                  |
         Browser client      Native client      Hosted client
         Rust/WASM + GPU     Rust + terminal    same native binary
               |                  |             over SSH or mosh
       render packages      ANSI cell diffs            |
               |                  |                    PTY
          browser screen      local terminal       remote terminal
```

## 2. Product Vision

V2 should feel like one authored interactive system rather than a terminal application copied into several transports.

The browser is the highest-fidelity client. It provides smooth local animation, GPU scene rendering, advanced lighting, custom render packages, pointer and touch input, and semantic browser accessibility.

The native terminal client is a thin, downloadable application with the same navigation, content, map behavior, themes, and animation state. It renders locally and connects directly to the backend.

SSH and mosh launch that same native client in an isolated server-side container. They do not use a separate screen-rendering backend and do not ask the application server to generate terminal frames.

The expected experience order is:

1. Browser for maximum visual fidelity and smooth animation.
2. Direct native terminal client for the lowest terminal-path latency.
3. Mosh for resilient remote terminal use.
4. SSH as the universal remote terminal baseline.

## 3. Problem Statement

The current application mixes rendering, state, data access, transport, and server authority. The browser migration also introduced a hybrid path that converts termap output through terminal-oriented buffers before uploading glyph cells to WebGL.

This causes:

- browser behavior and visuals that are difficult to prove equivalent to the legacy application;
- expensive scene construction, duplicate conversions, and avoidable per-frame allocation;
- MVT decoding, terrain preparation, and map rendering pressure on the browser main thread;
- asset-loading races that can omit terrain and administrative boundaries;
- visual glitches from dynamic glyph atlas generation and terminal assumptions;
- server-rendered SSH frames whose latency depends on network round trips and backpressure;
- divergent implementations for direct terminal, SSH, mosh, and browser clients;
- shader effects constrained by an already flattened terminal image;
- tests that pass reducers and builds without proving visual quality or interactive performance.

V2 replaces this architecture rather than extending the hybrid path.

## 4. Product Principles

1. Behavior is shared; transport and presentation are adapters.
2. The backend sends meaning, not pixels.
3. The browser main thread never performs heavy map or asset preparation.
4. Base visual parity is mechanical and testable.
5. Advanced rendering may radically change appearance without changing meaning.
6. Static screens consume effectively no CPU or GPU.
7. Incomplete asset generations never replace complete visible content.
8. Every shipped render package is trusted, bounded, recoverable application code.
9. Accessibility is generated from shared semantics, not inferred from pixels.
10. A feature is not complete until behavior, visual, performance, and failure-mode acceptance gates pass.

## 5. Goals

1. Match the approved legacy look in the browser through a canonical base render package.
2. Eliminate visible glyph corruption, incomplete map generations, input stalls, and animation hitching.
3. Keep browser interaction local and visible on the next display frame.
4. Support full browser rendering packages that can replace rasterization, lighting, shadows, materials, and composition.
5. Make dark and light base themes logically identical across browser and terminal clients.
6. Move application rendering and animation out of the backend.
7. Ship a downloadable thin native terminal client.
8. Run the same native client inside isolated SSH and mosh sessions.
9. Use one typed, versioned, resumable protocol for every client.
10. Publish vector, terrain, font, artwork, and render resources as immutable revisioned assets.
11. Preserve existing portfolio content, map tours, project scenes, procedural artwork, and Ask capabilities.
12. Establish release-blocking visual and performance benchmarks.

## 6. Non-Goals

- Pixel-identical font rasterization across arbitrary terminal emulators.
- Sending rendered cells or ANSI through the backend protocol.
- Running protected AI credentials or paid tools in clients.
- Allowing visitors or backend events to provide arbitrary shader source.
- Making advanced browser packages available in terminals when the terminal cannot represent them.
- Requiring all render packages to resemble a terminal.
- Maintaining compatibility with internal V1 rendering APIs.
- Preserving the current browser hybrid renderer after V2 acceptance.

## 7. Success Definition

V2 succeeds when:

- the browser base package is accepted against fixed legacy reference scenes;
- the canonical cell surface is byte-identical between native and WASM engines for equivalent inputs;
- the browser sustains smooth approved animation on reference hardware without main-thread long tasks;
- maps retain complete vector and terrain generations while moving;
- country and state boundaries are always present at their authored zooms;
- relief, draping, lighting, depth, and building placement use the same terrain source;
- browser render packages can replace base rasterization rather than only filter its output;
- direct native, SSH, and mosh clients use the same client engine and structured backend protocol;
- static scenes stop scheduling frames;
- dark and light base modes preserve identical layout, hierarchy, geometry, and interaction behavior.

## 8. Client Experience Requirements

### 8.1 Browser

- Keyboard, pointer, wheel, trackpad, and touch actions update local state immediately.
- Navigation and already-loaded content continue during backend disconnection.
- The active render package may be changed without reloading the application.
- Package failure falls back to the canonical base package without losing state.
- Map camera movement never waits for network acknowledgement.
- Missing incoming map data does not blank or partially replace the last complete frame.
- Reduced motion disables camera flights, temporal noise, persistence, and nonessential motion.
- Browser accessibility remains useful when GPU rendering is unavailable.

### 8.2 Direct Native Client

- A single downloadable binary connects to a configured V2 backend endpoint.
- Navigation, layout, animation, map state, Ask editing, and rendering run locally.
- The binary discovers truecolor, Unicode, keyboard, mouse, and terminal-size capabilities.
- Unsupported color or glyph capabilities degrade deterministically.
- Intermediate animation frames may be dropped when terminal throughput is lower than the simulation rate.
- Client upgrades and protocol incompatibility produce explicit guidance rather than corrupted output.
- Launch artifacts cover Linux x86_64 and aarch64, macOS x86_64 and arm64, and Windows x86_64.
- Release artifacts are reproducible where practical, signed where the platform supports it, and published with SHA-256 checksums and provenance.
- The compressed client artifact is under 25 MiB, idle non-map RSS is under 40 MiB, and total map-session memory remains under a tested 150 MiB bound.
- Installation supports a documented one-command path plus direct archive download, with explicit endpoint and update configuration.

### 8.3 SSH And Mosh

- SSH and mosh launch the same native V2 client binary used for direct connections.
- The hosted client runs in a restricted container or equivalent sandbox.
- The hosted client connects to the backend over a private loopback or service network.
- The PTY transports terminal output only; application data uses the structured backend protocol.
- Session resume does not duplicate Ask requests, messages, or paid work.
- SSH admission, mosh bootstrap, and application authorization remain server responsibilities.
- Hosted SSH and mosh images embed the same release artifact digest as the corresponding downloadable Linux client, except for an explicitly documented platform build difference.
- PTY resize, capability negotiation, disconnect, resume, output backpressure, authorization failure, and duplicate-work prevention have end-to-end tests.

## 9. System Architecture

V2 consists of independently testable layers:

```text
portfolio-domain
  immutable content and shared identifiers

portfolio-protocol
  versioned commands, events, snapshots, errors, capabilities

portfolio-client-core
  reducer, effects, animation, layout, interaction, semantic model

portfolio-scene
  renderer-neutral visual scene and canonical cell compositor

portfolio-map
  vector geometry, terrain, labels, camera, relief, map scene

portfolio-render-packages
  manifests, ABI, validation, built-in package resources

portfolio-browser
  WASM adapter, workers, WebGPU/WebGL renderer, accessibility mirror

portfolio-native
  protocol adapter, terminal capabilities, ANSI diff renderer

portfolio-backend
  content, sessions, AI, tools, persistence, asset publication

portfolio-map-build
  vector and terrain archive generation, validation, revisioning
```

Dependency direction must remain one-way. Backend code cannot depend on client renderers. Shared client code cannot depend on DOM, WebGL, WebGPU, Crossterm, Ratatui, sockets, files, processes, environment variables, or system clocks.

CI must enforce these boundaries through crate dependency checks and protocol-schema tests. Protocol fixtures must fail if a server message introduces viewport dimensions, terminal capabilities, cells, ANSI, GPU resources, render-package internals, or rendered output.

## 10. Ownership Boundaries

### 10.1 Client-Owned State

- section, history, selection, and focus;
- project, gallery, and taste navigation;
- local scroll and momentum;
- map camera, tilt, bearing, tour, search UI, and hover;
- input editing and optimistic submission state;
- viewport and layout class;
- deterministic animation clocks;
- selected theme, skin, render package, and bounded package parameters;
- loaded asset, vector tile, terrain tile, and render caches;
- local accessibility focus and announcements;
- connection and recoverable component status.

### 10.2 Backend-Owned State

- published portfolio content and revisions;
- AI execution, credentials, tools, and budgets;
- authoritative conversations, messages, and event history;
- request idempotency and cancellation state;
- visits and consented telemetry;
- geocoding and protected external search;
- session admission, authentication, and retention;
- immutable asset catalogue and revision publication.

### 10.3 Synchronized Data

- bootstrap content and capabilities;
- session snapshots and ordered events;
- semantic map, project, artwork, and diagram presentations;
- asset references and integrity metadata;
- public recoverable errors.

## 11. Deterministic Client Engine

The shared client engine accepts normalized actions and returns explicit effects.

```rust
pub enum LocalAction {
    Navigate(Section),
    Key(Key),
    Pointer(Pointer),
    Resize(LayoutMetrics),
    Tick(Duration),
    SetTheme(ThemeId),
    SetRenderPackage(PackageId),
    SetPackageParameter(ParameterId, ParameterValue),
    AssetReady(AssetId, AssetHandle),
    MapDataReady(MapGeneration),
    Server(ServerEvent),
}

pub enum Effect {
    Send(ClientCommand),
    RequestAsset(AssetRequest),
    RequestMap(MapDemand),
    Persist(ClientPreference),
    OpenExternal(ApprovedUrl),
    WakeAt(ClientInstant),
}
```

Requirements:

- Updates are deterministic for a supplied clock and inputs.
- Rendering is pure with respect to application state.
- I/O occurs only through effects and platform adapters.
- Reducers never start fetches, create workers, read files, or mutate render resources.
- Hit regions are generated by the same layout that draws their visual target.
- Animation scheduling declares when another visible change can occur.
- The engine can be replayed from action traces in native tests and WASM tests.

## 12. Shared Visual Contract

The client engine produces two projections from the same state snapshot.

### 12.1 Semantic View

The semantic view contains stable IDs, hierarchy, headings, prose, controls, values, focus, selection, live-region changes, transcript state, map descriptions, and approved actions.

It is consumed by browser accessibility, native screen-reader hints where available, tests, and non-GPU fallback presentation.

### 12.2 Visual Scene

The visual scene preserves authored intent before final rasterization.

```rust
pub struct VisualScene {
    pub viewport: LogicalViewport,
    pub camera: SceneCamera,
    pub primitives: Vec<Primitive>,
    pub lights: Vec<Light>,
    pub materials: MaterialTable,
    pub clips: Vec<ClipRegion>,
    pub hits: Vec<HitRegion>,
    pub animation: AnimationState,
}

pub enum Primitive {
    GlyphRun(GlyphRun),
    CellArt(CellArt),
    Sprite(SpriteInstance),
    VectorPath(VectorPath),
    Mesh(MeshInstance),
    Terrain(TerrainInstance),
    ParticleField(ParticleField),
    CompositeGroup(CompositeGroup),
}
```

The scene contains logical geometry, semantic material IDs, palette roles, depth, normals where available, light-response parameters, motion data, clips, and interaction IDs. It contains no GPU objects or terminal-specific types.

### 12.3 Canonical Cell Surface

The shared scene library also provides the parity reference compositor:

```rust
pub struct Cell {
    pub glyph: GlyphId,
    pub foreground: Rgba8,
    pub background: Rgba8,
    pub material: MaterialId,
    pub layer: LayerId,
    pub depth: u16,
    pub flags: CellFlags,
}
```

The canonical cell surface is used by the terminal renderer, browser base package, logical goldens, and fallback behavior.

Ratatui is not part of this contract.

## 13. Browser Rendering Architecture

WebGPU is the preferred renderer. WebGL2 provides a reduced-capability fallback. A semantic HTML fallback remains available when neither GPU API works.

The browser renderer owns:

- persistent GPU resources and staging buffers;
- immutable prebuilt glyph and sprite atlases;
- scene buffer packing;
- render-package graph validation and execution;
- package quality tiers and capability fallback;
- context/device-loss recovery;
- final composition and presentation;
- frame timing and bounded renderer diagnostics.

The renderer must not:

- construct application state;
- fetch undeclared network resources;
- execute backend-provided shader source;
- mutate reducer state during rendering;
- recreate buffers, atlases, pipelines, or uniform locations every frame;
- rasterize the production font dynamically;
- update semantic DOM on unchanged frames.

## 14. Full Render Packages

A V2 render package is a complete, versioned scene-rendering implementation. It is not merely a post-process shader.

A package may define:

- which scene primitives it consumes;
- how glyphs, cells, paths, sprites, meshes, and terrain are rasterized;
- material models and palette lookup;
- projection and snapping rules;
- geometry expansion and sprite selection;
- light types and light evaluation;
- shadow generation and filtering;
- terrain shading and atmospheric treatment;
- transparency and ordered composition;
- temporal simulation and history;
- post-processing and final presentation;
- package-specific bounded user parameters;
- reduced-motion and reduced-quality behavior.

The renderer provides a stable package ABI with approved inputs:

```text
visual scene buffers
canonical cell surface
glyph and sprite atlases
vector paths and mesh buffers
terrain elevation and normal tiles
material and semantic IDs
depth and object IDs
motion vectors
theme palette
time, delta, frame, viewport, DPR, and cell metrics
approved package parameters
```

Packages may choose one of three pipeline classes:

| Pipeline class | Description |
|---|---|
| Canonical cell | Renders the shared cell surface directly and matches terminal composition. |
| Scene-native | Rasterizes visual-scene primitives itself and may produce a non-terminal presentation. |
| Hybrid | Uses canonical cells for selected layers and scene-native rendering for maps, portraits, effects, or backgrounds. |

Advanced packages may look radically different from the base package. They must preserve semantic content, interaction IDs, selected state, readable text, and accessibility output.

## 15. Pixel-Art Lighting Package

V2 must include at least one scene-native pixel-art package proving that render packages can replace the rendering model.

Requirements:

- render to a deliberate low-resolution logical target and upscale with nearest-neighbor sampling;
- use authored sprite sheets, glyph masks, vector-to-pixel rules, and deterministic pixel snapping;
- shade terrain from the published elevation and normal data;
- support directional sun, ambient sky, emissive materials, and bounded local lights;
- cast terrain shadows using the height field;
- cast building and scene-object shadows from shared geometry or authored proxy meshes;
- keep light and shadow edges stable during camera movement;
- use palette-aware light ramps rather than applying smooth photographic gradients after rendering;
- support normal maps for authored portraits, sprites, and selected scene materials;
- support water reflection, highlight, and shoreline treatment without changing map data;
- expose time-of-day, light direction, shadow softness, palette, and atmosphere as bounded presets;
- provide deterministic fixed-clock golden scenes;
- degrade to an unshadowed pixel-art tier when required GPU features or budgets are unavailable.

Accurate lighting means internally coherent light direction, normals, occlusion, and cast shadows within the stylized scene. It does not require physically based photorealism.

## 16. Built-In Render Package Library

The current required set is the base package, phosphor terminal, signal/VHS, and ink typewriter. Every package and quality tier must pass visual, accessibility, recovery, and performance gates. Each package supports the full dark/light and color/monochrome variant matrix; monochrome CRT and VHS variants may retain colored burn, phosphor, tracking, or dropout artifacts while keeping scene content monochrome.

| Package | Pipeline class | Launch requirement | Intent |
|---|---|---|---|
| Base dark/light | Canonical cell | Required | Cross-client parity and accessibility reference. |
| Pixel terrain | Scene-native | Deferred | Pixel art, terrain lighting, buildings, cast shadows, animated water. |
| Phosphor terminal | Hybrid | Required | Glyph-focused CRT or amber/green display with persistence and bloom. |
| LCD | Hybrid | Post-launch candidate | Subpixel grid, response time, limited contrast, controlled ghosting. |
| Ink typewriter | Native glyph/cell | Required | Uneven key impact, ribbon depletion, misregistration, absorption, physical ink spread, and paper fibers. |
| Color paper | Scene-native | Deferred | Pigment, watercolor pooling, print registration, variable type weight, physical ink spread, and map-first composition. |
| Newsprint | Scene-native | Post-launch candidate | Halftone materials, limited inks, rough type and image treatment. |
| Signal/VHS | Hybrid | Required | Scan timing, glitches, chroma behavior, dropout, tracking, temporal history, and color/monochrome signal paths. |

Ink glyphs have per-character temporal state. New glyphs arrive through typewriter impact and absorb into the paper; removed or replaced glyphs dry and bleed out before their history is released. This lifecycle is part of glyph rasterization and may not be approximated by a full-screen overlay.

The backend publishes structured data and resources, never renderer output. A package owns how client scene data is interpreted. CRT, VHS, and ink intentionally choose the shared glyph/cell scene: they rasterize glyph coverage and cell materials themselves and never sample a pre-rendered canonical image. Future scene-native packages may instead choose vector paths, terrain, meshes, or sprites.

## 17. Render Package Format And Trust

Each package contains:

- package and renderer ABI versions;
- supported pipeline class;
- capability and fallback declarations;
- shader modules and entry points;
- primitive handlers and material declarations;
- static textures, atlases, meshes, and lookup tables;
- render passes and dependencies;
- resource formats, dimensions, lifetimes, and clear policy;
- history buffers and reset triggers;
- parameter definitions and approved presets;
- reduced-motion and quality-tier behavior;
- memory, pass, light, shadow, and particle budgets;
- golden fixture metadata.

Production executes only packages included in a compile-time allowlist. Package resources are embedded or published as same-origin immutable assets with pinned digests.

No server event, URL parameter, local-storage value, or visitor input may provide shader source, resource URLs, graph structure, or unbounded values.

Local development builds may support package hot reload. Production builds may not.

Package validation must reject:

- graph cycles and undeclared dependencies;
- unsupported resource formats;
- resource alias hazards;
- excessive target dimensions or memory;
- excessive pass, light, shadow, particle, or history counts;
- missing fallback chains;
- undeclared network or dynamic resource discovery;
- incompatible ABI versions.

## 18. Themes And Skins

Themes define renderer-independent design decisions:

- palette roles;
- typography roles;
- map layer colors and hierarchy;
- line widths and dash patterns;
- label ranks and contrast;
- relief strength and fog policy;
- selection, focus, warning, and status treatment;
- motion and reduced-motion defaults.

Render packages define how those roles become pixels.

Dark and light base themes must preserve:

- identical content and information hierarchy;
- identical logical layout and hit regions;
- identical map camera and geometry;
- identical label selection and placement;
- identical animation timing;
- contrast-compliant role mapping;
- byte-identical cell structure except for declared palette values.

The theme manifest explicitly declares the palette fields permitted to differ. Differential tests replay identical actions in dark and light mode and require equal semantic views, layout, hit regions, text, glyph IDs, labels, camera, selection, focus, animation state, depth, material IDs, and all non-palette cell data.

Advanced packages may add package-specific palettes and materials, but cannot silently alter navigation or content.

## 19. Map Data And Rendering

### 19.1 Vector Archive

Publish one custom revisioned PMTiles archive containing:

- country boundaries;
- state and province boundaries;
- coastlines;
- water and waterways;
- roads and rail;
- land use and land cover;
- buildings;
- places and administrative labels;
- landmarks and portfolio locations.

Country and state boundaries must be first-class vector layers with explicit administrative level, rank, minimum zoom, style role, and stable feature IDs.

`states.tmap` must not remain a required browser-side merge for production V2.

### 19.2 Terrain Archive

Publish a tiled multiresolution terrain archive rather than requiring clients to scan a monolithic heightmap.

Terrain tiles include:

- quantized elevation;
- minimum and maximum height;
- precomputed lower-resolution levels;
- normal or slope data where justified;
- nodata and water masks;
- bounds and integrity metadata.

The same terrain source drives:

- relief and hillshade;
- vector draping;
- terrain and building shadows;
- building placement and extrusion;
- depth and ridge occlusion;
- hover elevation and map inspection;
- scene-native package lighting.

### 19.3 Camera And Generations

- Camera state lives in the shared client engine.
- Cursor-anchored zoom and tilt-aware drag use shared projection math.
- Tours use deterministic authored trajectories and reduced-motion alternatives.
- A complete visible generation remains active until its replacement is complete for every mandatory layer over the projected viewport and guard band.
- Tile requests are derived from projected camera bounds, not pointer-event frequency.
- Hover, theme toggles, and unrelated actions do not restart acquisition.
- Missing tiles are negatively cached for a bounded revision lifetime.

A generation is complete only when every demanded vector and terrain tile has reached a terminal state of ready, declared-empty, or recoverable-error with an approved fallback. Progressive replacement is not allowed for mandatory geometry, boundaries, labels, or terrain. Optional detail layers may fade in later only when their omission cannot change label placement, picking, occlusion, or required geometry. Timeout, worker failure, revision change, and missing-tile behavior must retain the previous complete generation and expose bounded status rather than assembling a mixed frame.

The bootstrap includes a revision-matched overview asset under 100 KiB with country outline, state-boundary overview, water mask, and top-rank places. It is the initial map presentation until the first complete demanded generation is ready. When a camera moves outside the previous generation's guard band, covered geometry may continue to transform, while uncovered regions use an explicit neutral loading ground and never reuse geographically stale features. The first complete generation must arrive within 1.5 seconds on the normal network profile and 4 seconds on the constrained profile; otherwise the overview remains visible with bounded loading or recoverable-error status.

## 20. Browser Workers And Data Flow

Heavy browser work runs outside the main thread.

### 20.1 Asset Worker

- PMTiles header and directory parsing;
- HTTP range requests;
- request coalescing and cancellation;
- immutable range cache;
- decompression;
- integrity validation;
- terrain archive access;
- optional bounded IndexedDB persistence.

### 20.2 Scene Worker

- MVT decoding;
- terrain tile decoding;
- geometry projection and clipping;
- relief and terrain-cell generation for canonical mode;
- label placement;
- map picking structures;
- map scene and canonical cell composition;
- generation completeness reporting.

Workers transfer immutable typed arrays. `SharedArrayBuffer` may be used only when deployment headers and browser support permit it. A transferable double-buffer path is required.

The main thread applies completed results, advances lightweight client state, submits GPU work, and updates changed accessibility semantics.

## 21. Browser Performance Requirements

Reference desktop targets:

| Metric | Requirement |
|---|---:|
| Cached startup to usable frame | under 250 ms |
| Uncached startup on normal broadband | under 1.5 s excluding optional map detail |
| Input to visible local feedback | next display frame |
| Main-thread application work | under 4 ms p95 |
| Main-thread application work | under 8 ms p99, under 16 ms maximum in normal interaction |
| Base GPU frame | under 4 ms p95 |
| Advanced package GPU frame | under 10 ms p95 at selected quality tier |
| Sustained base animation | 60 FPS |
| Sustained advanced animation | 60 FPS reference, 30 FPS bounded fallback |
| End-to-end input latency | under 32 ms p95, under 50 ms p99 |
| Late or dropped frames in an accepted 60 FPS scene | under 1% over ten minutes |
| Frame pacing | p99 inter-frame interval under 25 ms at the 60 FPS tier |
| Main-thread long tasks during interaction | none over 50 ms, zero caused by map decode or terrain preparation |
| Idle frame scheduling | zero RAF after settling |
| Duplicate decode of cached tile | zero |
| Map request caused by hover-only movement | zero |
| Blank frame during map generation change | zero |
| Glyph corruption, atlas bleed, stale geometry, or visible layout shift | zero in golden and soak traces |

Additional requirements:

- Quality adaptation changes only declared package tiers.
- Quality changes never alter semantic content or input response.
- Frame queues keep the latest state and discard obsolete intermediate work.
- Worker results include generation IDs and stale generations are ignored.
- Caches are bounded by bytes, not only item counts.
- Performance instrumentation is excluded from hot paths when disabled.

The initial reference matrix is Apple M1 with 8 GB RAM on current stable Chrome and Safari at 1440x900 DPR 2, and a four-core Intel-class integrated-GPU laptop with 8 GB RAM on current stable Chrome and Firefox at 1366x768 DPR 1. Mobile acceptance uses Pixel 6-class Chrome and iPhone 12-class Safari for responsive layout and the declared bounded quality tier. Normal broadband means 50 Mbps downstream, 10 Mbps upstream, 30 ms RTT, and no packet loss. Constrained-network tests use 10 Mbps downstream, 100 ms RTT, 1% packet loss. Exact versions, power state, thermal state, run length, sample count, and percentile calculation are pinned in the benchmark harness.

At the mobile or constrained 30 FPS tier, main-thread work is under 8 ms p95 and 16 ms p99, GPU work is under 20 ms p95, input-to-present is under 50 ms p95, late or dropped frames remain under 2%, and the p99 inter-frame interval is under 45 ms. The renderer must prefer a declared lower package tier over irregular oscillation between 30 and 60 FPS.

## 22. Native Terminal Renderer

The native renderer consumes the canonical cell surface and emits terminal diffs.

Requirements:

- no Ratatui types in shared core or scene contracts;
- persistent previous-frame buffer;
- changed-run grouping and cursor movement minimization;
- deterministic truecolor and indexed-color quantization;
- Unicode capability fallback tables;
- bounded output queue;
- intermediate animation-frame dropping under backpressure;
- idle stop when the client engine reports no future visual change;
- terminal resize without backend involvement;
- optional local asset cache;
- same action trace and semantic state as the browser base package.

Ratatui may be used temporarily inside the native adapter during migration, but it is not the V2 shared rendering model and is not required in the final core.

Terminal performance budgets on the reference machine and normal network profile are:

| Client path | Cached startup after transport establishment | Input-to-present p95 | Resume after connection recovery |
|---|---:|---:|---:|
| Direct native | under 750 ms | under 25 ms | under 1.5 s |
| Mosh hosted client | under 1.5 s | under 80 ms | under 1.0 s after mosh recovery |
| SSH hosted client | under 1.5 s | under 120 ms | under 2.0 s after reconnect |

The terminal output queue holds at most two presentation frames, discards obsolete intermediate frames, and presents the latest state within 250 ms during backpressure. No path may queue unbounded terminal output or delay input processing behind stale animation frames.

## 23. Backend And Protocol

The backend exposes:

```text
GET  /api/v2/bootstrap
GET  /api/v2/session              WebSocket upgrade
GET  /assets/v2/...               immutable assets
GET  /map/v2/...                  range-capable map archives
POST /api/v2/telemetry            optional bounded telemetry
GET  /api/v2/health
```

The production protocol uses CBOR. JSON remains available for fixtures, diagnostics, and development.

Every server envelope contains:

- protocol version;
- session ID;
- monotonically increasing sequence;
- event payload;
- optional request ID;
- public timestamp or ordering metadata where required.

Protocol requirements:

- renderer-independent messages;
- explicit capabilities and compatibility negotiation;
- replay after a known sequence;
- snapshot fallback when replay is unavailable;
- durable request idempotency;
- replayable cancellation and terminal states;
- bounded message and collection sizes;
- public errors without secrets, internal commands, or unrestricted stack traces;
- compatibility fixtures consumed by backend, native client, and WASM client tests.

## 24. Ask, Tools, And Semantic Panels

- Question editing is entirely local.
- Submission sends one idempotent request.
- Answers stream as typed text and tool events.
- Map, project, and diagram results are semantic presentations, not screenshots.
- Cancellation is immediate locally and authoritative when acknowledged.
- Reconnect resumes from the last applied sequence.
- Duplicate request IDs never restart paid work.
- Completed conversations can be replayed by any V2 client.
- Browser and terminal render the same transcript state through their own renderers.

## 25. Asset Publication

All production assets are immutable and revisioned.

The bootstrap catalogue includes:

- asset ID;
- revision and content digest;
- URL;
- media type;
- byte length;
- range support requirement;
- bounds and zoom range where applicable;
- dependency and compatibility versions.

Map and terrain endpoints must support:

- `HEAD`;
- byte ranges;
- `206 Partial Content`;
- exact `Content-Range`;
- `Accept-Ranges: bytes`;
- stable ETag;
- identity representation for range requests;
- immutable cache control;
- bounded request and response sizes.

## 26. Accessibility

- The semantic view is the accessibility source of truth.
- Browser navigation, headings, controls, transcript, selections, and status are represented in semantic DOM.
- Semantic DOM updates only when semantic state changes.
- GPU output is not scraped to infer text or controls.
- Keyboard-only operation covers every section and package selector.
- Reduced motion disables nonessential scene and package motion.
- Increased contrast has explicit theme/package behavior.
- No information is communicated only through color, light, depth, or animation.
- A non-GPU fallback retains content, navigation, Ask, and map descriptions.

## 27. Security And Privacy

- AI credentials and protected tools remain backend-only.
- Render packages are compile-time allowlisted reviewed code.
- Production has no arbitrary shader editor or remote shader registry.
- Asset digests are pinned and validated where required.
- Content Security Policy restricts script, worker, and asset origins.
- Worker messages and decoded collections are size-bounded.
- Native clients validate endpoints, certificates, protocol versions, and message sizes.
- Hosted SSH/mosh clients run with least privilege and no user shell.
- Telemetry is optional, negotiated, sampled, bounded, and independent of operation.
- Telemetry never includes question text, answer text, map queries, precise pointer traces, credentials, or persistent fingerprinting identifiers.

## 28. Reliability And Recovery

- GPU device or context loss preserves client and session state.
- Package compile or allocation failure follows a declared fallback chain.
- The final fallback is the canonical base package or semantic HTML, never a black screen.
- Worker crash restarts the worker and retains the last complete map generation.
- Corrupt map or terrain data produces a bounded recoverable error.
- Network loss preserves loaded sections and local navigation.
- Protocol reconnect does not duplicate side effects.
- Resize, DPR, package, theme, quality, and long suspension reset temporal resources deterministically.

## 29. Testing And Release Gates

### 29.1 Logical Parity

For fixed content, actions, clock, viewport metrics, map data, and theme:

- native and WASM reducers produce identical state;
- native and WASM scene builders produce identical visual scenes;
- native and WASM canonical compositors produce byte-identical cell surfaces;
- semantic views are identical.

### 29.2 Visual Goldens

Fixed browser and native fixtures cover:

- all sections;
- compact, standard, and wide layouts;
- dark and light base themes;
- country, state, city, relief, half-3D, and full-3D map cameras;
- every built-in render package and quality tier;
- reduced motion and increased contrast;
- loading, offline, reconnect, and recoverable error states;
- fixed-clock animation phases.

The V1 reference set is captured before V2 implementation from named commits, fixed content and map revisions, bundled fonts, fixed clocks, and approved camera states. The base browser package is compared under a pinned software-rendered reference environment using exact pixels. Hardware browser runs allow only documented rasterization variance: no geometry, clipping, glyph, text, or layout differences; no more than 0.1% changed pixels; and no channel delta above 3/255 outside approved antialiasing edge masks. Chrome, Firefox, and Safari each run the viewport and DPR matrix. Any tolerance-mask change requires reviewer approval and a stored before/after artifact.

### 29.3 Render Package Tests

- manifest and ABI validation;
- graph-cycle and hazard rejection;
- shader compile and pipeline creation;
- resource and memory budget enforcement;
- fallback-chain behavior;
- resize and device-loss recovery;
- deterministic fixed-clock captures;
- text legibility and contrast;
- shadow direction and occlusion fixtures;
- terrain lighting continuity during camera movement;
- reduced-motion behavior;
- effects-disabled semantic completeness.

The pixel terrain package must prove scene-native execution automatically. Its primary color target must be produced from `VisualScene` primitive, material, terrain, light, and shadow inputs without sampling the canonical flattened cell color target as its primary image. Tests inspect the validated graph and fixed captures for terrain and building cast shadows, light-source coherence, normals, palette ramps, animated water, camera-stable shadow edges, and the declared unshadowed fallback tier.

### 29.4 Performance Tests

- browser main-thread frame traces;
- worker decode and generation timing;
- GPU timing where available;
- map request and decode counts;
- cache memory and eviction;
- ten-minute interaction soak;
- terminal changed-byte and frame-queue benchmarks;
- direct native, mosh, and SSH latency measurements;
- cold and warm startup measurements.

Performance release reports include p50, p95, p99, maximum frame time, late-frame rate, input-to-present latency, layout shifts, GPU/device errors, worker restarts, request counts, decode counts, and memory high-water marks. The ten-minute soak includes continuous pan, zoom, tilt, package switching, theme switching, section navigation, resize, background suspension, and recovery. No deterministic trace may exhibit glyph corruption, atlas bleed, stale generation replacement, black frames, or unbounded memory growth.

### 29.5 Theme And Map Differential Tests

- Dark and light action traces differ only in manifest-declared palette values.
- Country and state boundary fixtures cover every authored zoom transition and archive seam.
- Map fixtures cover projection edges, pan, zoom, bearing, tilt, and generation replacement.
- Terrain fixtures cover archive seams, nodata, coast and water edges, terrain/vector alignment, ridge occlusion, and drape continuity.
- Building fixtures require placement, extrusion, lighting, and shadows to agree with the same canonical elevation source.
- Stable IDs, label hierarchy, picking, and administrative ranks remain consistent across tile boundaries and quality tiers.

### 29.6 Download And Hosted Client Tests

- Every launch OS and architecture artifact starts, validates its checksum/provenance metadata, and connects to a production-compatible V2 test backend.
- Install, endpoint configuration, upgrade warning, incompatible-protocol handling, and uninstall documentation are exercised in clean environments.
- Direct native, hosted SSH, and hosted mosh sessions replay the same action and server-event traces.
- Hosted artifact digest, sandbox policy, PTY resize, disconnect/resume, authorization, backpressure, and idempotency are release gates.

A feature cannot be marked complete when only unit tests and compilation pass.

## 30. Migration Strategy

The current production route remains available until V2 passes acceptance.

Temporary deployment:

```text
/                       current production
/v2                     V2 browser client
/api/v2/...              V2 structured backend
/map/v2/...              V2 map and terrain archives
```

Migration stages:

1. Freeze V1 visuals as reference fixtures and record performance baselines.
2. Introduce V2 protocol, domain model, and deterministic client engine.
3. Introduce visual scene and canonical cell surface without Ratatui dependencies.
4. Build native ANSI diff renderer and prove canonical parity.
5. Build persistent browser GPU base package and prove base parity.
6. Build combined vector archive with country and state boundaries.
7. Build tiled terrain archive and browser worker map pipeline.
8. Port Experience with complete vector, terrain, labels, tours, and interaction.
9. Port remaining sections and Ask semantic panels.
10. Build pixel terrain package with lighting and shadows.
11. Add the remaining approved package library.
12. Ship downloadable native client.
13. Switch SSH and mosh to the same hosted native client.
14. Run visual, accessibility, reliability, and performance acceptance.
15. Promote `/v2` to `/` with a rollback route for one release window.
16. Remove V1 ANSI streaming and the hybrid browser renderer after the rollback window.

Each stage must deliver one complete vertical slice. Placeholder visuals do not replace accepted V1 sections.

V1 and V2 protocols remain independently versioned during rollout. Existing conversations and authoritative session records are migrated or read through a compatibility adapter with reconciliation tests; they are never silently discarded. Client preferences with a defined V2 equivalent are imported once, while unsupported shader or renderer preferences fall back explicitly. Public content URLs retain redirects or stable equivalents.

Promotion uses an internal cohort, opt-in public cohort, staged traffic ramp, and then default-route cutover. Each ramp has error, visual, latency, session-resume, and resource thresholds. Exceeding a threshold returns traffic to the previous stage. Rollback must preserve messages, conversations, request-id records, and asset-revision references created during V2 operation. V1 backend compatibility remains available for a declared window long enough for installed native clients to receive an upgrade notice. V1 removal requires evidence that supported native versions have crossed the compatibility floor.

Before the first public cohort, maintainers approve and pin the rollout thresholds, cohort sizes, dashboards, and rollback commands in a versioned release plan. The initial rollback triggers are any duplicate paid request, any confirmed data loss, crash-session rate above 0.1%, unrecovered session error rate above 1%, or p95 latency more than 10% over the applicable PRD budget for fifteen minutes. The V1 protocol compatibility window is at least 30 days after V2 becomes the default and may close only after supported native clients have received and displayed an upgrade path.

## 31. V2 Acceptance Criteria

V2 is ready for production only when all of the following are true:

- the browser base package passes approved legacy parity scenes;
- dark and light base themes pass logical cross-client parity;
- no browser path depends on Ratatui buffers or ANSI parsing;
- no backend route sends rendered frames or viewport-specific cells;
- CI dependency and protocol-schema boundary checks pass;
- browser navigation and animation require no server round trip;
- country and state boundaries render from the canonical vector archive;
- terrain, relief, draping, shadows, and depth use the canonical terrain archive;
- administrative zoom, seam, projection, terrain alignment, and building-shadow fixtures pass;
- browser p95, p99, maximum, frame-pacing, dropped-frame, input-latency, memory, and long-task budgets pass;
- static scenes stop rendering after settling;
- map hover does not trigger network acquisition;
- incomplete map generations never replace complete ones;
- the pixel terrain package passes automated scene-native rasterization, coherent lighting, cast-shadow, stability, and fallback tests;
- the required base, pixel terrain, and phosphor launch packages pass every declared quality tier;
- package failures recover to the canonical base package;
- signed or checksummed native artifacts pass installation and connection tests on every launch target;
- direct native, SSH, and mosh use the same V2 client engine, protocol, and verified hosted artifact build;
- session replay and idempotency prevent duplicate paid work;
- accessibility, reduced motion, keyboard operation, and semantic fallback pass;
- all release performance budgets pass on defined reference devices;
- mixed V1/V2 protocol operation, data migration, traffic ramp, and rollback data safety pass;
- rollback has been tested before route promotion.

## 32. Required Follow-Up ADRs

This PRD requires focused architecture decisions for:

1. Canonical visual scene and cell-surface ABI.
2. WebGPU primary renderer and WebGL2 fallback boundary.
3. Full render-package ABI, trust model, and resource budgets.
4. Pixel-art lighting and shadow model.
5. Combined vector archive schema and build pipeline.
6. Tiled terrain archive format and sampling contract.
7. Browser worker topology and transferable data model.
8. V2 protocol encoding, replay, and durable idempotency.
9. Native ANSI diff renderer and capability fallbacks.
10. SSH/mosh hosted-client isolation and backend authentication.
11. Golden-scene parity policy and reference hardware.
12. Asset revisioning, integrity, caching, and deployment headers.
