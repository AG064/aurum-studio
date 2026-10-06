// Optional local hosting for an exported game. No project-control or write endpoints.
import { createServer } from "node:http";
import { createReadStream } from "node:fs";
import { realpath, stat } from "node:fs/promises";
import { resolve, sep, extname } from "node:path";
const root = await realpath(resolve(process.argv[2] || "../dist/web"));
const types = { ".html": "text/html; charset=utf-8", ".js": "application/javascript", ".wasm": "application/wasm", ".json": "application/json", ".png": "image/png", ".svg": "image/svg+xml", ".txt": "text/plain; charset=utf-8" };
const server = createServer(async (request, response) => {
    try {
        if (!["GET", "HEAD"].includes(request.method) || request.url.length > 4096) { response.writeHead(405); response.end(); return; }
        const path = decodeURIComponent(new URL(request.url, "http://127.0.0.1").pathname);
        const candidate = resolve(root, "." + (path === "/" ? "/index.html" : path));
        if (!candidate.startsWith(root + sep)) { response.writeHead(403); response.end(); return; }
        const file = await realpath(candidate);
        if (!file.startsWith(root + sep) || !(await stat(file)).isFile()) { response.writeHead(403); response.end(); return; }
        response.writeHead(200, { "Content-Type": types[extname(file)] || "application/octet-stream", "X-Content-Type-Options": "nosniff", "Cache-Control": "no-store" });
        if (request.method === "HEAD") response.end();
        else createReadStream(file).on("error", () => response.destroy()).pipe(response);
    } catch { if (!response.headersSent) response.writeHead(404); response.end(); }
});
server.listen(0, "127.0.0.1", () => console.log(`PLAY http://127.0.0.1:${server.address().port}/`));
