// Revision URLs are the cache keys. Mutable catalogue/bootstrap requests always
// revalidate; an offline bootstrap may reuse the last successfully validated body.
const MEMORY_LIMIT = 32 * 1024 * 1024;
const DISK_LIMIT = 96 * 1024 * 1024;
export class AssetCache {
  memory = new Map();
  pending = new Map();
  bytes = 0;
  disk = globalThis.caches?.open("portfolio-v2-assets-v1").catch(() => null);
  writes = Promise.resolve();
  stagingBytes = 0;

  async get(url, limit, immutable = true) {
    if (immutable && this.memory.has(url)) {
      const bytes = this.memory.get(url);
      this.memory.delete(url);
      this.memory.set(url, bytes);
      return bytes;
    }
    if (this.pending.has(url)) return this.pending.get(url);
    const promise = this.load(url, limit, immutable);
    this.pending.set(url, promise);
    try {
      return await promise;
    } finally {
      this.pending.delete(url);
    }
  }

  async load(url, limit, immutable) {
    const disk = await this.disk;
    let response = immutable && disk ? await disk.match(url) : null;
    if (!response) {
      try {
        response = await fetch(url, {
          cache: immutable ? "force-cache" : "no-cache",
          signal: AbortSignal.timeout(15000),
        });
        if (!response.ok) throw new Error(`HTTP ${response.status}: ${url}`);
      } catch (error) {
        if (!immutable && disk) response = await disk.match(url);
        if (!response) throw error;
      }
    }
    if (Number(response.headers.get("content-length")) > limit)
      throw new Error("asset exceeds byte budget");
    const reader = response.body?.getReader();
    const parts = [];
    let size = 0;
    if (reader) {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        size += value.byteLength;
        if (size > limit) {
          await reader.cancel();
          throw new Error("asset exceeds byte budget");
        }
        parts.push(value);
      }
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const part of parts) {
      bytes.set(part, offset);
      offset += part.length;
    }
    if (size <= MEMORY_LIMIT) {
      while (this.bytes + size > MEMORY_LIMIT || this.memory.size >= 512) {
        const key = this.memory.keys().next().value;
        this.bytes -= this.memory.get(key).byteLength;
        this.memory.delete(key);
      }
      if (this.memory.has(url)) this.bytes -= this.memory.get(url).byteLength;
      this.memory.set(url, bytes);
      this.bytes += size;
    }
    // Persistence is best effort; queued writes must not retain an unbounded
    // second copy of every prefetched tile behind a slow Cache Storage backend.
    if (disk && this.stagingBytes + size <= MEMORY_LIMIT) {
      this.stagingBytes += size;
      this.writes = this.writes
        .then(async () => {
          await disk.put(
            url,
            new Response(bytes, {
              headers: { "content-length": String(size) },
            }),
          );
          const keys = await disk.keys();
          let total = 0;
          for (let i = keys.length - 1; i >= 0; i--) {
            const stored = await disk.match(keys[i]);
            total += Number(stored.headers.get("content-length"));
            if (total > DISK_LIMIT || i < keys.length - 512)
              await disk.delete(keys[i]);
          }
        })
        .catch(() => {}) // quota/private browsing must not prevent operation
        .finally(() => {
          this.stagingBytes -= size;
        });
    }
    return bytes;
  }
}
