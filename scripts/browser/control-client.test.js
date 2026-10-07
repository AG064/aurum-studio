import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { createControlClient } from "./control-client.js";

async function server(t, handler) {
    const instance = createServer(handler);
    await new Promise(resolve => instance.listen(0, "127.0.0.1", resolve));
    t.after(async () => {
        instance.closeAllConnections();
        await new Promise(resolve => instance.close(resolve));
    });
    return `http://127.0.0.1:${instance.address().port}`;
}
test("real loopback requests preserve results without retaining secrets", async t => {
    const origin = await server(t, (request, response) => {
        assert.equal(request.headers["x-aurum-token"], "fixture-secret");
        response.end(JSON.stringify({ ok: true, value: 42 }));
    });
    const client = createControlClient(origin, "fixture-secret");
    assert.equal((await client.request("/api/project", { op: "scene_inspect", project: "private-project" })).value, 42);
    const report = client.diagnostics();
    assert.equal(report.entries[0].phase, "finished");
    assert.equal(report.entries[0].operation, "scene_inspect");
    assert.equal(report.entries[0].status, 200);
    assert.doesNotMatch(JSON.stringify(report), /fixture-secret|private-project|headers|body/);
});
test("pending requests are visible and bounded before response headers", async t => {
    const origin = await server(t, () => {});
    const client = createControlClient(origin, "fixture-secret");
    const waiting = client.request("/api/project", { op: "scene_inspect" }, { budgetMs: 100 });
    assert.equal(client.diagnostics().entries[0].phase, "pending");
    await assert.rejects(waiting, /scene_inspect \/api\/project exceeded its 100ms request budget/);
    assert.equal(client.diagnostics().entries[0].failure, "budget_exhausted");
});
test("the deadline covers a response body that never completes", async t => {
    const origin = await server(t, (_, response) => {
        response.writeHead(200, { "Content-Type": "application/json" });
        response.write('{"ok":');
    });
    const client = createControlClient(origin, "fixture-secret");
    await assert.rejects(client.request("/api/preview", null, { budgetMs: 500 }), /request budget/);
    assert.equal(client.diagnostics().entries[0].failure, "budget_exhausted");
});
test("backend failures retain the verdict but redact echoed tokens", async t => {
    const origin = await server(t, (_, response) => response.end('{"ok":false,"error":"refused fixture-secret"}'));
    const client = createControlClient(origin, "fixture-secret");
    await assert.rejects(client.request("/api/preview", {}), /refused \[redacted\]/);
    assert.equal(client.diagnostics().entries[0].failure, "operation_failed");
});
test("raw discovery objects pass, while strict verdict callers require ok", async t => {
    const origin = await server(t, (_, response) => response.end('{"op":"play","contract_version":1}'));
    const raw = createControlClient(origin, "fixture-secret");
    assert.equal((await raw.request("/api/project", { op: "describe" })).contract_version, 1);
    const strict = createControlClient(origin, "fixture-secret", { requireOk: true });
    await assert.rejects(strict.request("/api/project", { op: "describe" }), /Control operation failed/);
});
test("HTTP failure cannot pass through a positive verdict", async t => {
    const origin = await server(t, (_, response) => { response.writeHead(500); response.end('{"ok":true}'); });
    const client = createControlClient(origin, "fixture-secret");
    await assert.rejects(client.request("/api/preview"), /Control operation failed/);
});
test("invalid JSON is recorded without storing its body", async t => {
    const origin = await server(t, (_, response) => response.end("private-invalid-body"));
    const client = createControlClient(origin, "fixture-secret");
    await assert.rejects(client.request("/api/preview"), /Non-JSON control result/);
    assert.equal(client.diagnostics().entries[0].failure, "invalid_json");
    assert.doesNotMatch(JSON.stringify(client.diagnostics()), /private-invalid-body/);
});
test("external origins, credential URLs, unsafe paths and budgets are refused", async () => {
    assert.throws(() => createControlClient("https://example.test", "secret"), /loopback/);
    assert.throws(() => createControlClient("http://user:pass@127.0.0.1", "secret"), /loopback/);
    assert.throws(() => createControlClient("http://127.0.0.1", ""), /token/);
    const client = createControlClient("http://127.0.0.1:1", "secret");
    await assert.rejects(client.request("/api/preview?token=private"), /path/);
    await assert.rejects(client.request("/api/preview", null, { budgetMs: Infinity }), /budget/);
    assert.equal(client.diagnostics().entries.length, 0);
});
