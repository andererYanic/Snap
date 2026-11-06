// Cloudflare Worker für Snap-Freigabelinks.
//
// Liefert die Download-Seite und reicht verschlüsselte Daten aus R2 durch. Der Worker kennt
// den Schlüssel nie (er steht im Link nach „#“ und wird vom Browser nicht übertragen).
// Er prüft Ablauf und – wenn ein KV-Namespace „COUNTS“ gebunden ist – die Höchstzahl der
// Downloads (Zählung näherungsweise). Abgelaufene Links werden beim Abruf gelöscht.

const ID = /^[0-9a-f]{32}$/;

const SECURITY_HEADERS = {
  "Content-Security-Policy":
    "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'",
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "no-referrer",
  "X-Frame-Options": "DENY",
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Resource-Policy": "same-origin",
};

function reply(body, status = 200, extra = {}) {
  return new Response(body, {
    status,
    headers: { "Cache-Control": "no-store", "Content-Type": "text/plain; charset=utf-8", ...SECURITY_HEADERS, ...extra },
  });
}

async function loadPolicy(env, id) {
  const obj = await env.BUCKET.get(`links/${id}/policy.json`);
  if (!obj) return null;
  let p;
  try {
    p = JSON.parse(await obj.text());
  } catch {
    return null;
  }
  if (p.v !== 1 || !Number.isSafeInteger(p.expires) || !Number.isSafeInteger(p.size_enc)) return null;
  return p;
}

async function removeLink(env, id) {
  await env.BUCKET.delete([`links/${id}/policy.json`, `links/${id}/meta`, `links/${id}/data`]);
  if (env.COUNTS) await env.COUNTS.delete(id);
}

/** Bytebereich (nur ein einfacher Bereich) → R2-Range. */
export function parseRange(header, size) {
  if (!header) return null;
  const m = /^bytes=(\d{0,20})-(\d{0,20})$/.exec(header.trim());
  if (!m || (m[1] === "" && m[2] === "")) return "invalid";
  let start, end;
  if (m[1] === "") {
    const n = Number(m[2]);
    if (n === 0) return "invalid";
    start = Math.max(0, size - n);
    end = size - 1;
  } else {
    start = Number(m[1]);
    end = m[2] === "" ? size - 1 : Math.min(Number(m[2]), size - 1);
  }
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start > end || start >= size) return "invalid";
  return { offset: start, length: end - start + 1 };
}

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (request.method !== "GET" && request.method !== "HEAD") return reply("Nur GET.\n", 405, { Allow: "GET, HEAD" });

    // Download-Seite
    const page = /^\/l\/([^/]+)$/.exec(url.pathname);
    if (page) {
      if (!ID.test(page[1])) return reply("Nicht gefunden.\n", 404);
      const html = await env.ASSETS.fetch(new Request(new URL("/link.html", url), { method: "GET" }));
      return new Response(html.body, {
        status: 200,
        headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store", ...SECURITY_HEADERS },
      });
    }

    const api = /^\/api\/([^/]+)\/(policy|meta|data)$/.exec(url.pathname);
    if (!api) {
      // Statische Dateien (app.js, snap-crypto.js, style.css) kommen aus den Assets.
      if (env.ASSETS && /^\/(app\.js|snap-crypto\.js|style\.css)$/.test(url.pathname)) return env.ASSETS.fetch(request);
      return reply("Nicht gefunden.\n", 404);
    }
    const [, id, part] = api;
    if (!ID.test(id)) return reply("Nicht gefunden.\n", 404);
    const policy = await loadPolicy(env, id);
    if (!policy) return reply("Nicht gefunden.\n", 404);
    const now = Math.floor(Date.now() / 1000);
    if (policy.expires <= now) {
      const cleanup = removeLink(env, id);
      if (ctx && ctx.waitUntil) ctx.waitUntil(cleanup);
      else await cleanup;
      return reply("Abgelaufen.\n", 410);
    }
    // Download-Grenze: Neue Besucher (Seite/Metadaten, erstes Stück) werden abgewiesen, ein
    // bereits begonnener Download darf aber zu Ende laden.
    const used = env.COUNTS && policy.max_downloads ? Number((await env.COUNTS.get(id)) || 0) : 0;
    const limitReached = !!(env.COUNTS && policy.max_downloads && used >= policy.max_downloads);
    if (limitReached && part !== "data") return reply("Download-Grenze erreicht.\n", 410);

    if (part === "policy") {
      const pub = { v: 1, expires: policy.expires, password: !!policy.password, size_enc: policy.size_enc };
      return reply(JSON.stringify(pub), 200, { "Content-Type": "application/json" });
    }
    if (part === "meta") {
      const obj = await env.BUCKET.get(`links/${id}/meta`);
      if (!obj) return reply("Nicht gefunden.\n", 404);
      return reply(obj.body, 200, { "Content-Type": "application/octet-stream" });
    }

    // Daten (verschlüsselt), mit Bytebereichen.
    const range = parseRange(request.headers.get("Range"), policy.size_enc);
    if (range === "invalid") return reply("Bereich ungültig.\n", 416, { "Content-Range": `bytes */${policy.size_enc}` });
    const starts = !range || range.offset === 0;
    if (limitReached && starts) return reply("Download-Grenze erreicht.\n", 410);
    const obj = await env.BUCKET.get(`links/${id}/data`, range ? { range } : {});
    if (!obj) return reply("Nicht gefunden.\n", 404);
    // Ein Download zählt beim Abruf des ersten Stücks.
    if (env.COUNTS && policy.max_downloads && starts) {
      await env.COUNTS.put(id, String(used + 1), { expiration: Math.max(policy.expires, now + 60) });
    }
    const headers = { "Content-Type": "application/octet-stream", "Accept-Ranges": "bytes" };
    if (range) {
      headers["Content-Range"] = `bytes ${range.offset}-${range.offset + range.length - 1}/${policy.size_enc}`;
      headers["Content-Length"] = String(range.length);
      return reply(request.method === "HEAD" ? null : obj.body, 206, headers);
    }
    headers["Content-Length"] = String(policy.size_enc);
    return reply(request.method === "HEAD" ? null : obj.body, 200, headers);
  },
};
