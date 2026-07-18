/**
 * Static dev server for the unified UI (ui/index.html) backed by the
 * in-browser neural engine only: no /api endpoints, so the page's exact-
 * database option stays hidden. Same URL layout as `ninemm serve`:
 *   /            -> ../ui/index.html   (the one shared frontend)
 *   /nn/*        -> dist/*             (compiled TS modules)
 *   /export/*    -> export/*           (model.json / model.bin)
 */
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { basename, extname, join } from "node:path";

const ROOT = join(import.meta.dirname, "..");
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".map": "application/json",
  ".bin": "application/octet-stream",
  ".onnx": "application/octet-stream",
};

function resolve(urlPath) {
  if (urlPath === "/" || urlPath === "/index.html") return join(ROOT, "..", "ui", "index.html");
  // Single-segment file names only, mirroring src/server.rs's static routes.
  if (urlPath.startsWith("/nn/")) return join(ROOT, "dist", basename(urlPath));
  if (urlPath.startsWith("/export/")) return join(ROOT, "export", basename(urlPath));
  return null;
}

const port = Number(process.argv[2] ?? 8834);
const server = createServer(async (req, res) => {
  const urlPath = decodeURIComponent(new URL(req.url, "http://x").pathname);
  const filePath = resolve(urlPath);
  try {
    if (filePath === null) throw new Error("outside served roots");
    const data = await readFile(filePath);
    res.setHeader("Content-Type", MIME[extname(filePath)] ?? "application/octet-stream");
    res.end(data);
  } catch {
    res.statusCode = 404;
    res.end("not found");
  }
});
server.listen(port, () => console.log(`serving the unified UI (neural engine) on http://localhost:${port}`));
