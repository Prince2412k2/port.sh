# V2 implementation and release acceptance

The end-to-end client/server application is implemented for Home, Experience,
Projects, Skills, Taste, and Ask. The existing production deployment remains
available while release acceptance is completed.

## Implemented application paths

- One backend publishes profile, project, experience, and Taste content, asset
  revisions, cacheable vector/elevation data, and authoritative conversations.
  It never constructs terminal frames or browser pixels.
- Direct native and hosted SSH/mosh clients run the same downloadable binary.
  Input, navigation, animation, camera state, editing, and rendering are local.
- The browser owns a WASM worker engine and WebGPU/WebGL2 cell renderers.
  Heavy decode/preparation/composition does not run on its main thread.
- Projects preserve the authored marks, tool strips, and engineering diagrams.
  Skills preserves the lifting logo sheet, drag/throw motion, and optional drift.
  Taste preserves the authored gallery, room backgrounds, pictures, and quotes.
  Compact project layouts retain the actual content instead of cropping it out.
- Ask has local editing, streaming answers, bounded semantic map/project/diagram
  tools, protected optional web retrieval, cancellation, durable request IDs,
  restart reconciliation, conversation replay, and binary CBOR WebSocket updates.
  `/reach` writes a durable human message without calling the AI provider.
- Native clients persist their resume code locally. Hosted clients are ephemeral
  and never share a visitor's conversation through a common home-directory file;
  an explicit resume code can be supplied to the forced SSH command.
- Vector and elevation tiles install together, atomically, after the complete
  demanded generation is available. Clients fetch small elevation tiles with
  guard bands instead of downloading/scanning the complete regional heightmap.
- Encoded, decoded, persistent, request, output-frame, and temporal caches have
  explicit bounds. Visible demand precedes tour prefetch. Malformed vector data
  is validated before decoding and does not become a WASM trap.
- Base rendering uses persistent GPU cell instances and changed-range uploads.
  Glyph coverage is prebaked from the bundled font during publication; no dynamic
  production glyph rasterization is needed.
- CRT and VHS evaluate glyph coverage themselves. Ink evaluates glyph impact,
  ribbon variation, misregistration, absorption, fibre-dependent spreading, and
  bounded removal history. Settled base/ink/CRT scenes stop scheduling frames.
- Experimental pixel mode rasterizes terrain/vector/building geometry with a
  depth buffer, directional normals, palette ramps, height-field cast shadows,
  and bounded-rate animated water. It uses shared labels/markers without paying
  for the full canonical map geometry rasterizer. Building coverage depends on
  the supplied archive/footprints; geometry is never invented for missing regions.
- Fingerprinted web bundles keep scripts, workers, fonts, atlases, and WASM on the
  same immutable build. Worker restart preserves local selection/camera/draft
  state and retains the previous complete map while assets are reacquired.

## Verification

- Workspace tests and optimized native/WASM builds pass.
- Real native/WASM execution compared 39 canonical frames byte-for-byte across
  all six sections, multiple layout sizes, themes, and reduced motion.
- The production glyph atlas covers every glyph in that replay trace.
- The installed/downloaded native client and hosted SSH/mosh clients pass real
  PTY startup, section navigation, resize, return-home, exit, and restoration
  checks. The hosted/downloadable executable digests match.
- HTTP checks exercise bootstrap compatibility, ETags, conditional requests,
  immutable ranges/If-Range, actual MVT delivery, tiny elevation tiles, coordinate
  bounds, and coherent fingerprinted bundles.
- An instrumented local AI fixture exercises actual provider streaming, CBOR
  decoding by the real WASM client, semantic tools, duplicate/conflicting IDs,
  cancellation, restart/replay, and prevention of duplicate work after a crash.
- DevTools exercises the actual browser UI, local editing and message delivery,
  map navigation, renderer switching, idle settling, offline navigation, and
  WebGL context restoration.
- A dedicated Chromium instance exercises actual WebGPU pipeline creation and
  renderer switching. GPU readback verifies nonblank glyph and presentation
  targets. Its Vulkan/SwiftShader compositor does not provide a valid visual or
  hardware performance reference, so its screenshots/timings are not release
  goldens or passing performance budgets.

## Release gates still outstanding

These are distinct from the implemented application paths:

1. Approved V1 visual references and the full hardware/browser/DPR/theme/package
   matrix, ten-minute soaks, and PRD latency/memory/frame-pacing budgets.
2. Cross-platform install/connection execution, signing/provenance publication,
   and the supported-client compatibility window. CI builds the launch targets;
   Linux execution was verified here.
3. Production cohort thresholds, V1 conversation import/read-through policy,
   default-route cutover, and rollback validation. V1 records are preserved by
   leaving the existing deployment/data intact; they are not silently imported
   into unrelated visitor capability-token sessions.
4. The final strict PRD crate boundary: canonical authored rendering still uses
   pure termap/skysheet/Ratatui buffer adapters inside client code. No backend or
   browser main-thread path uses them. A fully Ratatui-free canonical compositor
   and combined administrative/vector archive remain architectural follow-ups.
5. Full pixel-art lighting/art-direction acceptance and broader building data.
   The experimental geometry/shadow path is implemented, but should not be
   described as a fully approved game-quality package.

Run the checks in README.md against the latest build. This document does not
declare the entire draft PRD production-accepted.
