// Download-Seite für Snap-Freigabelinks. Der Schlüssel steht im Link nach „#“ und verlässt
// den Browser nie; der Server liefert nur verschlüsselte Daten.
import { CHUNK, TAG, chunkCount, deriveKeys, encryptedSize, openChunk, openMeta } from "/snap-crypto.js";

const $ = (id) => document.getElementById(id);
const status = (t) => ($("status").textContent = t);

function human(n) {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1000 && i < u.length - 1) {
    n /= 1000;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}

async function getBytes(url, range) {
  const r = await fetch(url, { headers: range ? { Range: range } : {}, cache: "no-store" });
  if (r.status === 410) throw new Error("Dieser Link ist abgelaufen oder wurde widerrufen.");
  if (!r.ok) throw new Error(`Fehler beim Laden (${r.status}).`);
  return new Uint8Array(await r.arrayBuffer());
}

async function main() {
  const m = location.pathname.match(/^\/l\/([0-9a-f]{32})$/);
  const secret = location.hash.slice(1);
  if (!m || !secret) throw new Error("Unvollständiger Link – bitte den ganzen Link kopieren (inklusive #…).");
  const id = m[1];
  const pr = await fetch(`/api/${id}/policy`, { cache: "no-store" });
  if (pr.status === 410 || pr.status === 404) throw new Error("Dieser Link ist abgelaufen oder wurde widerrufen.");
  if (!pr.ok) throw new Error(`Fehler beim Laden (${pr.status}).`);
  const policy = await pr.json();
  const metaBytes = await getBytes(`/api/${id}/meta`);

  const unlock = async (password) => {
    status("Wird entschlüsselt …");
    const keys = await deriveKeys(secret, id, password);
    const meta = await openMeta(keys, metaBytes);
    if (encryptedSize(meta.size) !== policy.size_enc) throw new Error("Daten passen nicht zusammen.");
    return { keys, meta };
  };

  let session;
  if (policy.password) {
    $("info").textContent = "Passwortgeschützte Datei";
    $("pwform").hidden = false;
    session = await new Promise((resolve) => {
      $("pwform").addEventListener("submit", async (e) => {
        e.preventDefault();
        try {
          resolve(await unlock($("pw").value));
          $("pwform").hidden = true;
        } catch (err) {
          status(err.message);
        }
      });
    });
  } else {
    session = await unlock(null);
  }
  const { keys, meta } = session;
  document.title = meta.name;
  $("name").textContent = meta.name;
  const until = new Date(policy.expires * 1000).toLocaleString("de-DE");
  $("info").textContent = `${human(meta.size)} · verfügbar bis ${until}`;
  status("");
  $("download").hidden = false;
  $("download").addEventListener("click", () => download(id, keys, meta).catch((e) => status(e.message)), { once: true });
}

async function download(id, keys, meta) {
  $("download").hidden = true;
  $("progress").hidden = false;
  const n = chunkCount(meta.size);
  const step = CHUNK + TAG;
  // Große Dateien direkt auf die Platte schreiben, wo der Browser das kann; sonst im Speicher sammeln.
  let writable = null;
  if (window.showSaveFilePicker && meta.size > 200e6) {
    try {
      const handle = await window.showSaveFilePicker({ suggestedName: meta.name });
      writable = await handle.createWritable();
    } catch {
      writable = null;
    }
  }
  const parts = [];
  const total = encryptedSize(meta.size);
  const PER_REQUEST = 16; // 16 Stücke ≈ 1 MiB je Abruf
  for (let first = 0; first < n; first += PER_REQUEST) {
    const last = Math.min(n, first + PER_REQUEST) - 1;
    const start = first * step;
    const end = Math.min(total, (last + 1) * step) - 1;
    const bytes = await getBytes(`/api/${id}/data`, `bytes=${start}-${end}`);
    if (bytes.length !== end - start + 1) throw new Error("Unvollständige Daten erhalten.");
    for (let i = first; i <= last; i++) {
      const off = (i - first) * step;
      const plain = await openChunk(keys, i, i === n - 1, bytes.subarray(off, Math.min(bytes.length, off + step)));
      if (writable) await writable.write(plain);
      else parts.push(plain);
    }
    $("progress").value = (last + 1) / n;
    status(`${Math.round(((last + 1) / n) * 100)} %`);
  }
  if (writable) {
    await writable.close();
  } else {
    const url = URL.createObjectURL(new Blob(parts, { type: meta.mime || "application/octet-stream" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = meta.name;
    document.body.appendChild(a);
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 60000);
  }
  status("Fertig – die Datei wurde gespeichert.");
}

main().catch((e) => status(e.message));
