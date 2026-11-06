// Kreuztest: entschlüsselt mit dem Browser-Code, was Snap (Rust) verschlüsselt hat.
// Aufruf: node decrypt.mjs <ordner>  (enthält secret, id, password?, meta.bin, data.bin)
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { CHUNK, TAG, chunkCount, deriveKeys, openChunk, openMeta } from "../public/snap-crypto.js";

const dir = process.argv[2];
const read = (f) => readFileSync(join(dir, f));
const secret = read("secret").toString().trim();
const id = read("id").toString().trim();
const password = existsSync(join(dir, "password")) ? read("password").toString() : null;
const keys = await deriveKeys(secret, id, password);
const meta = await openMeta(keys, new Uint8Array(read("meta.bin")));
const data = new Uint8Array(read("data.bin"));
const n = chunkCount(meta.size);
const out = [];
for (let i = 0; i < n; i++) {
  const start = i * (CHUNK + TAG);
  out.push(await openChunk(keys, i, i === n - 1, data.subarray(start, Math.min(data.length, start + CHUNK + TAG))));
}
writeFileSync(join(dir, "out.bin"), Buffer.concat(out));
writeFileSync(join(dir, "meta.json"), JSON.stringify(meta));
