// Tests des Workers mit nachgebautem R2/KV/Assets (node --test gateway/cloudflare/test).
import { test } from "node:test";
import assert from "node:assert/strict";
import worker, { parseRange } from "../worker.js";

const ID = "0123456789abcdef0123456789abcdef";

function makeEnv({ expires, max = null, withCounts = false } = {}) {
  const store = new Map();
  const data = new Uint8Array(1000).map((_, i) => i % 251);
  store.set(`links/${ID}/policy.json`, new TextEncoder().encode(
    JSON.stringify({ v: 1, expires: expires ?? Math.floor(Date.now() / 1000) + 3600, max_downloads: max, password: false, size_enc: data.length }),
  ));
  store.set(`links/${ID}/meta`, new Uint8Array([1, 2, 3]));
  store.set(`links/${ID}/data`, data);
  const counts = new Map();
  return {
    data,
    store,
    counts,
    env: {
      BUCKET: {
        async get(key, opts = {}) {
          const v = store.get(key);
          if (!v) return null;
          const slice = opts.range ? v.subarray(opts.range.offset, opts.range.offset + opts.range.length) : v;
          return { body: slice, text: async () => new TextDecoder().decode(slice) };
        },
        async delete(keys) {
          for (const k of [].concat(keys)) store.delete(k);
        },
      },
      ASSETS: { fetch: async () => new Response("<html>Seite</html>") },
      COUNTS: withCounts
        ? { get: async (k) => counts.get(k) ?? null, put: async (k, v) => counts.set(k, v), delete: async (k) => counts.delete(k) }
        : undefined,
    },
  };
}

const call = (env, path, init = {}) => worker.fetch(new Request("https://share.example.de" + path, init), env, null);

test("Seite und Sicherheitskopfzeilen", async () => {
  const { env } = makeEnv();
  const r = await call(env, `/l/${ID}`);
  assert.equal(r.status, 200);
  assert.match(r.headers.get("content-security-policy"), /default-src 'none'/);
  assert.equal(r.headers.get("x-frame-options"), "DENY");
  assert.equal((await call(env, "/l/../../etc")).status, 404);
  assert.equal((await call(env, "/l/ABC")).status, 404);
  assert.equal((await call(env, "/irgendwas")).status, 404);
  assert.equal((await call(env, `/api/${ID}/data`, { method: "POST" })).status, 405);
  assert.equal((await call(env, `/api/${ID}/data`, { method: "DELETE" })).status, 405);
  assert.equal((await call(env, `/api/${"f".repeat(32)}/policy`)).status, 404);
  assert.equal((await call(env, `/api/${ID}/secret`)).status, 404);
});

test("Regeln, Metadaten und Bereiche", async () => {
  const { env, data } = makeEnv();
  const p = await (await call(env, `/api/${ID}/policy`)).json();
  assert.equal(p.size_enc, 1000);
  assert.equal(p.max_downloads, undefined, "interne Angaben werden nicht ausgeliefert");
  const meta = new Uint8Array(await (await call(env, `/api/${ID}/meta`)).arrayBuffer());
  assert.deepEqual([...meta], [1, 2, 3]);
  const r = await call(env, `/api/${ID}/data`, { headers: { Range: "bytes=100-199" } });
  assert.equal(r.status, 206);
  assert.equal(r.headers.get("content-range"), "bytes 100-199/1000");
  assert.deepEqual(new Uint8Array(await r.arrayBuffer()), data.subarray(100, 200));
  const full = await call(env, `/api/${ID}/data`);
  assert.equal(full.status, 200);
  assert.equal((await full.arrayBuffer()).byteLength, 1000);
  for (const bad of ["bytes=1000-", "bytes=5-1", "bytes=-0", "bytes=-", "items=0-1", "bytes=0-1,5-6"]) {
    assert.equal((await call(env, `/api/${ID}/data`, { headers: { Range: bad } })).status, 416, bad);
  }
});

test("Abgelaufen: 410 und gelöscht", async () => {
  const { env, store } = makeEnv({ expires: Math.floor(Date.now() / 1000) - 1 });
  assert.equal((await call(env, `/api/${ID}/policy`)).status, 410);
  assert.equal(store.size, 0, "Objekte entfernt");
  assert.equal((await call(env, `/api/${ID}/data`)).status, 404);
});

test("Download-Grenze mit KV", async () => {
  const { env } = makeEnv({ max: 2, withCounts: true });
  for (let i = 0; i < 2; i++) {
    assert.equal((await call(env, `/api/${ID}/data`, { headers: { Range: "bytes=0-99" } })).status, 206);
    // Weitere Stücke desselben Downloads zählen nicht
    assert.equal((await call(env, `/api/${ID}/data`, { headers: { Range: "bytes=100-199" } })).status, 206);
  }
  assert.equal((await call(env, `/api/${ID}/data`, { headers: { Range: "bytes=0-99" } })).status, 410);
  assert.equal((await call(env, `/api/${ID}/data`)).status, 410, "ganze Datei = neuer Download");
  assert.equal((await call(env, `/api/${ID}/policy`)).status, 410);
  // Der zuletzt begonnene Download darf zu Ende laden
  assert.equal((await call(env, `/api/${ID}/data`, { headers: { Range: "bytes=200-299" } })).status, 206);
});

test("parseRange", () => {
  assert.deepEqual(parseRange("bytes=0-9", 100), { offset: 0, length: 10 });
  assert.deepEqual(parseRange("bytes=90-", 100), { offset: 90, length: 10 });
  assert.deepEqual(parseRange("bytes=-10", 100), { offset: 90, length: 10 });
  assert.deepEqual(parseRange("bytes=95-500", 100), { offset: 95, length: 5 });
  assert.equal(parseRange(null, 100), null);
  assert.equal(parseRange("bytes=99999999999999999999999-", 100), "invalid");
});
