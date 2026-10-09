// Exercise the shipped WASM adapter against real map assets: terminal-only
// preferences, bounded widescreen grids, and complete coarse/detail generations.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";

const endpoint = process.argv[2] || "http://127.0.0.1:8332";
const dist = resolve("v2/crates/browser/dist");
const { default: init, Engine } = await import(pathToFileURL(`${dist}/portfolio_v2_browser.js`));
await init({ module_or_path: await readFile(`${dist}/portfolio_v2_browser_bg.wasm`) });
async function bytes(path) {
  const response = await fetch(`${endpoint}${path}`);
  assert.equal(response.status, 200, path);
  return new Uint8Array(await response.arrayBuffer());
}
const engine = new Engine();
try {
  engine.bootstrap(await bytes("/api/v2/bootstrap"));
  engine.resize(1920, 1080, 1);
  engine.frame();
  let meta = JSON.parse(engine.metadata());
  assert.equal(meta.cols, 180);
  assert.equal(meta.rows, 47);
  for (const legacy of ["ink", "pixel"]) {
    engine.restore_appearance("dark", legacy, "color");
    engine.frame();
    assert.equal(JSON.parse(engine.metadata()).variant.package, "canonical");
  }
  for (const expected of ["crt", "vhs", "canonical"]) {
    engine.key("p");
    engine.frame();
    assert.equal(JSON.parse(engine.metadata()).variant.package, expected);
  }
  engine.key("i");
  engine.frame();
  assert.equal(JSON.parse(engine.metadata()).variant.package, "canonical");
  engine.reduced_motion(true);
  engine.key("2");
  const manifest = JSON.parse(new TextDecoder().decode(await bytes("/api/v2/map")));
  engine.overlay(await bytes(`${manifest.base}/states.tmap`));
  assert.equal(manifest.terrain_format, "tmhg-tiles-v1");
  engine.tiled_terrain(true);
  engine.frame();
  const preview = JSON.parse(engine.preview_demand());
  const demand = JSON.parse(engine.demand());
  assert(preview.length > 0 && preview.length < demand.length);
  async function load(tile, terrain = true) {
    const key = tile.join("/");
    if (!engine.has_tile(...tile)) assert(engine.tile(...tile, await bytes(`${manifest.base}/tiles/${key}`)));
    if (terrain && !engine.has_terrain_tile(...tile))
      assert(engine.terrain_tile(...tile, await bytes(`${manifest.base}/terrain/${key}`)));
  }
  for (const tile of preview) await load(tile, false);
  engine.frame();
  assert(JSON.parse(engine.preview_demand()).length > 0, "vectors alone must not install preview");
  for (const tile of preview) await load(tile);
  engine.frame();
  assert.deepEqual(JSON.parse(engine.preview_demand()), [], "complete preview was installed");
  assert.equal(JSON.parse(engine.metadata()).loading, true, "detail is still outstanding");
  for (const tile of demand) await load(tile);
  const frame = engine.frame();
  meta = JSON.parse(engine.metadata());
  assert.equal(meta.loading, false);
  assert.equal(frame.byteLength, meta.cols * meta.rows * 24);
  assert(meta.decodedBytes > 0);
  engine.key("6");
  engine.frame();
  assert.equal(JSON.parse(engine.metadata()).loading, false, "Ask must not display map loading status");
  console.log(`PASS: widescreen grid, terminal-only preferences, atomic ${preview.length}-tile preview → ${demand.length}-tile detail`);
} finally {
  engine.free();
}
