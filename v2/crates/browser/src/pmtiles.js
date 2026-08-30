let generation = 0;
const ranges = new Map();
const tiles = new Map();
const TILE_LIMIT = 256;
const BYTE_LIMIT = 32 * 1024 * 1024;
let tileBytes = 0;

export function beginMapGeneration() {
  generation++;
  return generation;
}

async function range(url, start, length, requestedGeneration) {
  if (requestedGeneration !== null && requestedGeneration !== generation) {
    throw new DOMException("stale map request", "AbortError");
  }
  const key = `${url}:${start}:${length}`;
  if (ranges.has(key)) return ranges.get(key);
  const pending = (async () => {
    const response = await fetch(url, { headers: { Range: `bytes=${start}-${start + length - 1}` } });
    if (response.status !== 206) throw new Error(`PMTiles range expected 206, got ${response.status}`);
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.length !== length) throw new Error("short PMTiles range");
    return bytes;
  })();
  ranges.set(key, pending);
  try {
    const bytes = await pending;
    if (ranges.size > 24) ranges.delete(ranges.keys().next().value);
    return bytes;
  } catch (error) {
    ranges.delete(key);
    throw error;
  }
}

async function unzip(bytes, gzip) {
  if (!gzip) return bytes;
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

function u64(view, offset) { return Number(view.getBigUint64(offset, true)); }
function varint(bytes, position) {
  let result = 0;
  let multiplier = 1;
  for (;;) {
    const byte = bytes[position.index++];
    result += (byte & 127) * multiplier;
    if (!(byte & 128)) return result;
    multiplier *= 128;
  }
}
function directory(bytes) {
  const position = { index: 0 };
  const count = varint(bytes, position);
  const entries = Array.from({ length: count }, () => ({ id: 0, run: 0, length: 0, offset: 0 }));
  let last = 0;
  for (const entry of entries) { last += varint(bytes, position); entry.id = last; }
  for (const entry of entries) entry.run = varint(bytes, position);
  for (const entry of entries) entry.length = varint(bytes, position);
  for (let index = 0; index < count; index++) {
    const value = varint(bytes, position);
    entries[index].offset = value === 0 && index
      ? entries[index - 1].offset + entries[index - 1].length
      : value - 1;
  }
  return entries;
}
function findEntry(entries, wanted) {
  let low = 0;
  let high = entries.length;
  while (low < high) {
    const middle = (low + high) >> 1;
    if (entries[middle].id <= wanted) low = middle + 1;
    else high = middle;
  }
  if (!low) return null;
  const entry = entries[low - 1];
  return entry.run === 0 || wanted < entry.id + entry.run ? entry : null;
}
function tileId(z, x, y) {
  let accumulated = 0;
  for (let level = 0; level < z; level++) accumulated += 2 ** (2 * level);
  let distance = 0;
  const size = 2 ** z;
  for (let step = size / 2; step >= 1; step /= 2) {
    const rx = (x & step) ? 1 : 0;
    const ry = (y & step) ? 1 : 0;
    distance += step * step * ((3 * rx) ^ ry);
    if (!ry) {
      if (rx) { x = size - 1 - x; y = size - 1 - y; }
      [x, y] = [y, x];
    }
  }
  return accumulated + distance;
}

async function readMapTile(url, z, x, y, requestedGeneration) {
  const key = `${z}/${x}/${y}`;
  if (tiles.has(key)) {
    const output = tiles.get(key);
    tiles.delete(key);
    tiles.set(key, output);
    return output;
  }
  const header = await range(url, 0, 127, requestedGeneration);
  const view = new DataView(header.buffer, header.byteOffset, header.byteLength);
  if (new TextDecoder().decode(header.slice(0, 7)) !== "PMTiles" || header[7] !== 3) {
    throw new Error("invalid PMTiles v3 header");
  }
  let offset = u64(view, 8);
  let length = u64(view, 16);
  const leafOffset = u64(view, 40);
  const dataOffset = u64(view, 56);
  const internalGzip = header[97] === 2;
  const tileGzip = header[98] === 2;
  const wanted = tileId(z, x, y);
  for (let depth = 0; depth < 4; depth++) {
    const entries = directory(await unzip(await range(url, offset, length, requestedGeneration), internalGzip));
    const entry = findEntry(entries, wanted);
    if (requestedGeneration !== null && requestedGeneration !== generation) {
      throw new DOMException("stale map request", "AbortError");
    }
    if (!entry) return undefined;
    if (!entry.run) { offset = leafOffset + entry.offset; length = entry.length; continue; }
    const output = await unzip(
      await range(url, dataOffset + entry.offset, entry.length, requestedGeneration),
      tileGzip,
    );
    tiles.set(key, output);
    tileBytes += output.byteLength;
    while (tiles.size > TILE_LIMIT || tileBytes > BYTE_LIMIT) {
      const oldest = tiles.keys().next().value;
      tileBytes -= tiles.get(oldest).byteLength;
      tiles.delete(oldest);
    }
    return output;
  }
  return undefined;
}

export async function fetchMapTile(url, z, x, y, requestedGeneration) {
  return readMapTile(url, z, x, y, requestedGeneration);
}

function idle() {
  return new Promise((resolve) => {
    if ("requestIdleCallback" in window) requestIdleCallback(resolve, { timeout: 750 });
    else setTimeout(resolve, 0);
  });
}

export async function prefetchMapTiles(url, wanted, onTile) {
  let next = 0;
  window.portfolioV2Prefetch = { planned: wanted.length, completed: 0, cachedTiles: tiles.size, cachedBytes: tileBytes };
  const worker = async () => {
    while (next < wanted.length) {
      const [z, x, y] = wanted[next++];
      await idle();
      try {
        const output = await readMapTile(url, z, x, y, null);
        if (output) onTile(z, x, y, output);
        window.portfolioV2Prefetch.completed++;
        window.portfolioV2Prefetch.cachedTiles = tiles.size;
        window.portfolioV2Prefetch.cachedBytes = tileBytes;
      } catch (error) {
        if (error?.name !== "AbortError") console.warn("map prefetch failed", error);
      }
    }
  };
  await Promise.all([worker(), worker()]);
  return window.portfolioV2Prefetch;
}
