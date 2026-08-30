# Portfolio V2

V2 is an isolated workspace and process. It does not import the V1 terminal renderer or the abandoned browser prototype.

## Crates

- `portfolio-v2-protocol`: bounded, renderer-independent wire types.
- `portfolio-v2-assets`: immutable authored and baked client assets.
- `portfolio-v2-scene`: renderer-neutral visual scene and canonical cell compositor.
- `portfolio-v2-client-core`: deterministic state, layout, semantics, and visual scene.
- `portfolio-v2-native`: direct client and ANSI cell-diff adapter.
- `portfolio-v2-browser`: thin WASM adapter with a WebGPU canonical-cell package, bounded DOM fallback, and semantic document.
- `portfolio-v2-backend`: content publication and `/api/v2` server.

## Run

```sh
cd v2/crates/browser
trunk build --release

cd ../..
cargo run -p portfolio-v2-backend
```

Open `http://127.0.0.1:8322/v2/`. The server listens on `0.0.0.0:8322` by default.

The backend reads `../portfolio/data/about.txt` by default. Override the content path with `PORTFOLIO_V2_ABOUT`, the browser distribution with `PORTFOLIO_V2_WEB_DIR`, and the listener with `PORTFOLIO_V2_ADDR`.
