// Bounded headless test requests. Diagnostics retain timing and operation names,
// never tokens, headers, request bodies, response bodies or private source.
export function createControlClient(origin, token, { budgetMs = 300000, requireOk = false } = {}) {
    const url = new URL(origin);
    if (url.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname) || url.origin !== origin) {
        throw new Error("Test control origin must be a plain loopback HTTP origin");
    }
    if (typeof token !== "string" || !token || token.length > 4096 || /[\r\n]/.test(token)) throw new Error("Invalid test control token");
    const entries = [];
    let dropped = 0;
    const request = async (path, body, options = {}) => {
        if (!/^\/api\/[a-z_]+$/.test(path)) throw new Error("Invalid test control path");
        const budget = options.budgetMs ?? budgetMs;
        if (!Number.isSafeInteger(budget) || budget < 1 || budget > 600000) throw new Error("Invalid test request budget");
        const operation = body?.op ?? body?.action;
        const item = { path, method: body ? "POST" : "GET", operation: typeof operation === "string" && /^[a-z_]{1,64}$/.test(operation) ? operation : null, budget_ms: budget, status: null, phase: "pending", failure: null };
        const started = performance.now();
        if (entries.length < 256) entries.push({ item, started });
        else dropped++;
        const signal = AbortSignal.timeout(budget);
        try {
            const response = await fetch(origin + path, {
                method: item.method,
                headers: { "Content-Type": "application/json", "X-Aurum-Token": token },
                ...(body ? { body: JSON.stringify(body) } : {}),
                signal,
            });
            item.status = response.status;
            item.phase = "reading_result";
            let result;
            try { result = await response.json(); }
            catch (error) {
                item.failure = signal.aborted ? "budget_exhausted" : "invalid_json";
                if (signal.aborted) throw error;
                throw new Error(`Non-JSON control result from ${path}`);
            }
            if (!result || typeof result !== "object" || Array.isArray(result)) {
                item.failure = "invalid_result";
                throw new Error(`Expected an object control result from ${path}`);
            }
            // File/discovery operations return raw objects, not an ok envelope.
            // Verdict-bearing operations and strict callers still require ok.
            if (!response.ok || result.ok === false || (requireOk && result.ok !== true)) {
                item.failure = "operation_failed";
                throw new Error(String(result?.error || "Control operation failed").replaceAll(token, "[redacted]"));
            }
            item.phase = "finished";
            return result;
        } catch (error) {
            item.failed_at = item.phase;
            item.phase = "failed";
            item.failure ??= signal.aborted ? "budget_exhausted" : "transport_failed";
            if (signal.aborted) throw new Error(`${item.operation || item.method} ${path} exceeded its ${budget}ms request budget`, { cause: error });
            throw error;
        } finally { item.elapsed_ms = Math.round(performance.now() - started); }
    };
    const diagnostics = () => ({
        entries: entries.map(({ item, started }) => ({ ...item, elapsed_ms: item.elapsed_ms ?? Math.round(performance.now() - started) })),
        dropped,
        includes_credentials: false,
    });
    return { request, diagnostics };
}
