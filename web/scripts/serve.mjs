import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join } from "node:path";

const ROOT = join(import.meta.dirname, "..");
const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".json": "application/json",
  ".map": "application/json",
  ".bin": "application/octet-stream",
  ".onnx": "application/octet-stream",
};

const port = Number(process.argv[2] ?? 8834);
const server = createServer(async (req, res) => {
  const urlPath = decodeURIComponent(new URL(req.url, "http://x").pathname);
  const filePath = join(ROOT, urlPath === "/" ? "/public/index.html" : urlPath);
  try {
    const data = await readFile(filePath);
    res.setHeader("Content-Type", MIME[extname(filePath)] ?? "application/octet-stream");
    res.end(data);
  } catch {
    res.statusCode = 404;
    res.end("not found");
  }
});
server.listen(port, () => console.log(`serving ${ROOT} on http://localhost:${port}`));
