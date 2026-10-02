import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { monitorHttp } from "./http-diagnostics.js";

const request = (url, body = {}) => ({ url: () => url, method: () => "POST", postDataJSON: () => body, failure: () => ({ errorText: `net::ERR_ABORTED ${url}` }) });

test("HTTP diagnostics omit tokens, headers, bodies and non-loopback requests", () => {
    const page = new EventEmitter();
    const report = monitorHttp(page);
    const local = request("http://127.0.0.1:1234/api/project?t=private-token", { op: "scene_inspect", text: "private source" });
    const external = request("https://example.org/api/project?t=other-token");
    page.emit("request", local);
    page.emit("requestfailed", local);
    page.emit("request", external);
    page.emit("response", { request: () => external, status: () => 200 });
    const result = report();
    assert.equal(result.entries.length, 1);
    assert.equal(result.entries[0].failure, "net::ERR_ABORTED");
    assert.equal(result.entries[0].operation, "scene_inspect");
    assert.doesNotMatch(JSON.stringify(result), /private-token|private source|other-token|example\.org/);
});

test("HTTP diagnostics are bounded and do not duplicate finished requests", () => {
    const page = new EventEmitter();
    const report = monitorHttp(page);
    for (let index = 0; index < 1002; index++) {
        const item = request("http://localhost:1234/api/preview", { action: "export" });
        page.emit("request", item);
        page.emit("response", { request: () => item, status: () => 200 });
        page.emit("requestfailed", item);
    }
    assert.equal(report().entries.length, 1000);
    assert.equal(report().dropped, 2);
    assert.equal(report().entries[0].status, 200);
});
