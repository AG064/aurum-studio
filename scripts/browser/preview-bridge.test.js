import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const source = await readFile(new URL("../../crates/aurum-studio-server/ui/preview-bridge.js", import.meta.url), "utf8");
function fixture() {
    const messages = [], calls = [];
    let tick;
    let payload = { ok: true, sha256: "a".repeat(64), values: { player_speed: 9 } };
    const window = {
        parent: { postMessage: message => messages.push(message) },
        addEventListener() {},
        aurumApplyTuning(text) { const input = JSON.parse(text); calls.push(input); window.aurumAppliedTuning = input.sha256; },
    };
    vm.runInNewContext(source, {
        window, document: { addEventListener() {} },
        AURUM_PREVIEW: { session: "fixture-session", parent: "http://127.0.0.1:1234" },
        setInterval(callback, interval) { assert.equal(interval, 500); tick = callback; },
        fetch: async () => ({ json: async () => payload }),
    });
    return { window, messages, calls, tick: () => tick(), setPayload: next => { payload = next; } };
}
test("a save-response race can reconcile a replayed acknowledgement without duplicate application", async () => {
    const bridge = fixture();
    await bridge.tick();
    assert.equal(bridge.calls.length, 1);
    const early = bridge.messages.find(message => message.type === "aurum-preview-tuning");
    assert.equal(early.sha256, "a".repeat(64));
    // Studio has not received its write response and ignores the first ack.
    bridge.messages.length = 0;
    await bridge.tick();
    assert.equal(bridge.calls.length, 1);
    assert.equal(bridge.messages.length, 1);
    assert.equal(bridge.messages[0].sha256, early.sha256);
    assert.equal(bridge.messages[0].session, "fixture-session");
});
test("late and stale runtime acknowledgements cannot claim that a new revision applied", async () => {
    const bridge = fixture();
    bridge.window.aurumApplyTuning = text => bridge.calls.push(JSON.parse(text));
    await bridge.tick();
    await bridge.tick();
    assert.equal(bridge.calls.length, 1);
    assert.equal(bridge.messages.length, 0);
    bridge.window.aurumAppliedTuning = "a".repeat(64);
    await bridge.tick();
    assert.equal(bridge.messages.length, 1);
    bridge.messages.length = 0;
    bridge.setPayload({ ok: true, sha256: "b".repeat(64), values: { player_speed: 10 } });
    await bridge.tick();
    assert.equal(bridge.calls.length, 2);
    assert.equal(bridge.messages.length, 0);
    bridge.window.aurumAppliedTuning = "b".repeat(64);
    await bridge.tick();
    assert.equal(bridge.messages[0].sha256, "b".repeat(64));
});
test("a failed source read or absent runtime hook never reports applied tuning", async () => {
    const bridge = fixture();
    bridge.window.aurumApplyTuning = undefined;
    await bridge.tick();
    assert.equal(bridge.messages.length, 0);
    bridge.window.aurumAppliedTuning = "a".repeat(64);
    bridge.setPayload({ ok: false, sha256: "a".repeat(64) });
    await bridge.tick();
    assert.equal(bridge.messages.length, 0);
});
