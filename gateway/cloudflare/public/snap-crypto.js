// Snap-Freigabelinks: Entschlüsselung im Browser (Format v1, siehe docs/LINK_FORMAT.md).
// Gegenstück zu core/src/links/crypto.rs – beide werden gegeneinander getestet.

export const CHUNK = 65536;
export const TAG = 16;
export const PBKDF2_ROUNDS = 600000;

const enc = new TextEncoder();

function b64urlDecode(s) {
  if (!/^[A-Za-z0-9_-]{43}$/.test(s)) throw new Error("Ungültiger Schlüssel im Link");
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "=";
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  if (out.length !== 32) throw new Error("Ungültiger Schlüssel im Link");
  return out;
}

async function hkdfKey(prk, info) {
  const bits = await crypto.subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt: prk.salt, info: enc.encode(info) },
    prk.key,
    256,
  );
  return crypto.subtle.importKey("raw", bits, { name: "AES-GCM" }, false, ["decrypt"]);
}

/** Leitet die Schlüssel ab: Geheimnis (Base64url aus dem Link), Link-ID, optional Passwort. */
export async function deriveKeys(secretB64, id, password) {
  const secret = b64urlDecode(secretB64);
  let ikm = secret;
  if (password) {
    const pwKey = await crypto.subtle.importKey("raw", enc.encode(password), "PBKDF2", false, ["deriveBits"]);
    const p = new Uint8Array(
      await crypto.subtle.deriveBits(
        { name: "PBKDF2", hash: "SHA-256", salt: enc.encode("snap-link-v1/pw/" + id), iterations: PBKDF2_ROUNDS },
        pwKey,
        256,
      ),
    );
    ikm = new Uint8Array(64);
    ikm.set(secret, 0);
    ikm.set(p, 32);
  }
  const key = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  const prk = { key, salt: enc.encode("snap-link-v1/" + id) };
  return { data: await hkdfKey(prk, "data"), meta: await hkdfKey(prk, "meta") };
}

function chunkNonce(index, last) {
  const n = new Uint8Array(12);
  let v = BigInt(index);
  for (let i = 10; i >= 3; i--) {
    n[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  n[11] = last ? 1 : 0;
  return n;
}

export function chunkCount(size) {
  return Math.max(1, Math.ceil(size / CHUNK));
}

export function encryptedSize(size) {
  return size + chunkCount(size) * TAG;
}

/** Entschlüsselt ein Stück. Wirft bei falschem Schlüssel oder Manipulation. */
export async function openChunk(keys, index, last, bytes) {
  if (bytes.length < TAG || bytes.length > CHUNK + TAG) throw new Error("Ungültige Stückgröße");
  const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: chunkNonce(index, last) }, keys.data, bytes);
  return new Uint8Array(plain);
}

/** Entschlüsselt die Metadaten (Name, Größe, Typ). */
export async function openMeta(keys, bytes) {
  if (bytes.length > 65536) throw new Error("Metadaten zu groß");
  let plain;
  try {
    plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: new Uint8Array(12) }, keys.meta, bytes);
  } catch {
    throw new Error("Falscher Schlüssel oder falsches Passwort");
  }
  const meta = JSON.parse(new TextDecoder().decode(plain));
  if (meta.v !== 1 || meta.chunk !== CHUNK || typeof meta.name !== "string" || !Number.isSafeInteger(meta.size)) {
    throw new Error("Unbekanntes Format");
  }
  return meta;
}
