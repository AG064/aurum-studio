// Lightweight request diagnostics for loopback APIs only. No credentials,
// query strings, headers, response bodies or project source are retained.
export function monitorHttp(page) {
    const started = new WeakMap();
    const entries = [];
    let dropped = 0;
    page.on("request", request => {
        const url = new URL(request.url());
        if (!["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) || !url.pathname.startsWith("/api/")) return;
        let operation = null;
        try {
            const body = request.postDataJSON();
            const value = body?.op ?? body?.action;
            if (typeof value === "string" && /^[a-z_]{1,64}$/.test(value)) operation = value;
        } catch { /* Non-JSON requests have no operation label. */ }
        started.set(request, { path: url.pathname, method: request.method(), operation, time: performance.now() });
    });
    const append = (request, status, failure = null) => {
        const item = started.get(request);
        if (!item) return;
        started.delete(request);
        if (entries.length >= 1000) { dropped++; return; }
        entries.push({ path: item.path, method: item.method, operation: item.operation, status, failure, elapsed_ms: Math.round(performance.now() - item.time) });
    };
    page.on("response", response => append(response.request(), response.status()));
    page.on("requestfailed", request => {
        const failure = request.failure()?.errorText || "";
        append(request, null, failure.match(/(?:net::)?ERR_[A-Z0-9_]+/)?.[0] || "request failed");
    });
    return () => ({ entries, dropped, includes_credentials: false });
}
