import { test, expect } from "@playwright/test";
import { mkdtemp, cp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { monitorHttp } from "./http-diagnostics.js";
import { openWorkspace } from "./workspace-ready.js";
import { createControlClient } from "./control-client.js";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
let work, project, child, context, page, endpoint, origin, token, diagnostics;
let output = "";
const errors = [];
let controlClient;
test.describe.configure({ mode: "serial", timeout: 360000 });
const control = (path, body, options) => controlClient.request(path, body, options);
// Use only the game's normal runtime interface, never injected gameplay state.
const runtime = async request => {
    const url = await page.locator("#preview-frame").getAttribute("src");
    const frame = page.frames().find(frame => frame.url() === url);
    if (!frame) throw new Error("Game frame is not loaded");
    const result = await frame.evaluate(async request => {
        if (typeof window.aurumRuntimeRequest !== "function") throw new Error("Runtime is not ready");
        window.aurumRuntimeResponse = "";
        window.aurumRuntimeRequest(JSON.stringify(request));
        const deadline = performance.now() + 2000;
        while (!window.aurumRuntimeResponse && performance.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20));
        return JSON.parse(window.aurumRuntimeResponse);
    }, request);
    if (!result.ok) throw new Error(result.error || "Runtime request failed");
    return result;
};
const state = async () => {
    const result = await runtime({ op: "checkpoint" });
    if (!result.ok || !result.checkpoint?.custom) throw new Error(result.error || "Custom checkpoint missing");
    return result.checkpoint.custom;
};
// Freeze through the same interface used by rebuilds so software rendering
// cannot starve browser capture. Always restore the original pause state.
const capture = async path => {
    const frozen = await runtime({ op: "checkpoint", freeze: true });
    try { await page.screenshot({ path, timeout: 30000 }); }
    finally { await runtime({ op: "resume", paused: frozen.checkpoint.paused }); }
    expect((await state()).paused).toBe(frozen.checkpoint.custom.paused);
};
test.beforeAll(async ({ browser }) => {
    for (const key of ["AURUM_BINARY", "AURUM_GODOT", "AURUM_WEB_TEMPLATE"]) if (!process.env[key]) throw new Error(`Missing ${key}`);
    work = await mkdtemp(join(tmpdir(), "aurum-relay-browser-"));
    project = join(work, "relay-yard");
    await cp(join(repo, "examples/relay-yard"), project, { recursive: true, filter: path => ![".aurum", ".godot", "dist", ".git"].includes(basename(path)) });
    child = spawn(resolve(process.env.AURUM_BINARY), ["studio", project, "--no-open"], {
        windowsHide: true,
        env: { ...process.env, AURUM_STUDIO_HOME: join(work, "state"), APPDATA: join(work, "userdata"), LOCALAPPDATA: join(work, "userdata"), XDG_DATA_HOME: join(work, "userdata") },
        stdio: ["ignore", "pipe", "pipe"],
    });
    child.stdout.on("data", data => output += data);
    child.stderr.on("data", data => output += data);
    child.on("error", error => output += error.message);
    await expect.poll(() => output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/)?.[0], { timeout: 30000 }).toBeTruthy();
    endpoint = output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/)[0];
    origin = new URL(endpoint).origin;
    token = new URL(endpoint).searchParams.get("t");
    controlClient = createControlClient(origin, token);
    context = await browser.newContext({ viewport: { width: 1488, height: 1056 } });
    page = await context.newPage();
    diagnostics = monitorHttp(page);
    page.on("pageerror", error => errors.push(error.message));
    page.on("console", message => {
        if (message.type() === "error" && /SCRIPT ERROR|Parse Error|TypeError|RuntimeError/.test(message.text())) errors.push(message.text());
    });
});
test.afterAll(async ({}, info) => {
    if (context) await context.close();
    if (origin && child?.exitCode === null) await control("/api/stop", {}, { budgetMs: 5000 }).catch(() => {});
    if (child?.exitCode === null) await Promise.race([new Promise(resolve => child.once("exit", resolve)), new Promise(resolve => setTimeout(resolve, 5000))]);
    if (child?.exitCode === null) child.kill();
    await writeFile(info.outputPath("studio.log"), token ? output.replaceAll(token, "[redacted]") : output);
    await writeFile(info.outputPath("evidence.json"), JSON.stringify({ work, project, errors }, null, 2));
    if (diagnostics) await writeFile(info.outputPath("http-diagnostics.json"), JSON.stringify(diagnostics(), null, 2));
    if (controlClient) await writeFile(info.outputPath("control-diagnostics.json"), JSON.stringify(controlClient.diagnostics(), null, 2));
});
test("3D imported assets render and keyboard play reaches the real game", async ({}, info) => {
    await openWorkspace(page, endpoint);
    await expect(page.locator("#project")).toHaveText("relay-yard");
    await page.locator("#run-web").click();
    await expect(page.locator("#preview-state")).toHaveText("Preview ready", { timeout: 180000 });
    await expect.poll(async () => (await state()).version, { timeout: 30000 }).toBe(3);
    const canvas = page.frameLocator("#preview-frame").locator("canvas");
    await canvas.click({ position: { x: 20, y: 20 } });
    await page.keyboard.press("Enter");
    await expect.poll(async () => (await state()).phase).toBe("play");
    const before = await state();
    await page.keyboard.down("w");
    try { await expect.poll(async () => (await state()).player.travelled, { timeout: 15000 }).toBeGreaterThan(3); }
    finally { await page.keyboard.up("w"); }
    expect((await state()).position[2]).toBeLessThan(before.position[2]);
    await page.keyboard.press("Space");
    await expect.poll(async () => (await state()).player.dash_count).toBe(1);
    await page.keyboard.press("q");
    await expect.poll(async () => (await state()).player.emp_count).toBe(1);
    const current = await state();
    const point = current.view.aim_points.find(point => point.visible && point.screen[0] > 40 && point.screen[0] < current.view.width - 40 && point.screen[1] > 40 && point.screen[1] < current.view.height - 40);
    expect(point, "A real enemy must be visible for aiming").toBeTruthy();
    const bounds = await canvas.boundingBox();
    await page.mouse.move(bounds.x + point.screen[0] / current.view.width * bounds.width, bounds.y + point.screen[1] / current.view.height * bounds.height);
    await page.mouse.down();
    try { await expect.poll(async () => (await state()).player.shots_fired).toBeGreaterThan(2); }
    finally { await page.mouse.up(); }
    await capture(info.outputPath("relay-yard-active.png"));
    await page.keyboard.press("Escape");
    await expect.poll(async () => (await state()).paused).toBe(true);
    await capture(info.outputPath("relay-yard-playing.png"));
    expect(errors).toEqual([]);
});
test("live tuning and source rebuilding preserve the combat mission", async ({}, info) => {
    if (!(await page.locator("#runtime-inspector summary").isVisible())) await page.locator("#toggle-inspector").click();
    if (!(await page.locator("#inspect-runtime").isVisible())) await page.locator("#runtime-inspector summary").click();
    await page.locator("#auto-rebuild").uncheck();
    await page.locator("#inspect-runtime").click();
    await expect(page.locator("#runtime-status")).toContainText("editable values");
    const speed = page.getByLabel("Live move_speed", { exact: true });
    await speed.fill("8");
    await speed.press("Tab");
    await expect(page.locator("#runtime-status")).toHaveText("move_speed applied without restarting.");
    const before = await state();
    const oldUrl = await page.locator("#preview-frame").getAttribute("src");
    await writeFile(info.outputPath("checkpoint-before.json"), JSON.stringify(before, null, 2));
    const source = await control("/api/project", { project, op: "read", path: "godot/yard.gd" });
    await control("/api/project", { project, op: "write", path: "godot/yard.gd", text: source.text + "\n# 3D state-preserving source revision.\n", expected_sha256: source.sha256 });
    // Hosted SwiftShader runs recorded a 38-second freshness response under
    // rendering load. Keep this eventual-status check bounded without changing
    // the required stale verdict or retrying a failed gameplay scenario.
    await expect(page.locator("#preview-revision")).toHaveText("Source changed", { timeout: 60000 });
    await page.locator("#run-web").click();
    await expect.poll(async () => {
        const text = await page.locator("#reload-status").textContent();
        if (/failed|unavailable/i.test(text)) throw new Error(text);
        return text;
    }, { timeout: 180000 }).toBe("Rebuilt and restored the checkpoint.");
    expect(await page.locator("#preview-frame").getAttribute("src")).not.toBe(oldUrl);
    const after = await state();
    expect(after.carried).toBe(before.carried);
    expect(after.powered).toEqual(before.powered);
    expect(after.paused).toBe(true);
    expect(after.player.health).toBe(before.player.health);
    expect(after.player.shots_fired).toBe(before.player.shots_fired);
    expect(after.enemies.map(enemy => [enemy.uid, enemy.health])).toEqual(before.enemies.map(enemy => [enemy.uid, enemy.health]));
    expect(after.tuning.move_speed).toBe(8);
    expect(after.position[0]).toBeCloseTo(before.position[0], 2);
    expect(after.position[2]).toBeCloseTo(before.position[2], 2);
    expect(after.elapsed).toBeGreaterThanOrEqual(before.elapsed);
    await capture(info.outputPath("relay-yard-restored.png"));
    expect(errors).toEqual([]);
});

test("pause resumes cleanly and presentation survives resizing", async ({}, info) => {
    const canvas = page.frameLocator("#preview-frame").locator("canvas");
    await canvas.click({ position: { x: 20, y: 20 } });
    await page.keyboard.press("Escape");
    await expect.poll(async () => (await state()).paused).toBe(false);
    await page.setViewportSize({ width: 1100, height: 800 });
    await capture(info.outputPath("relay-yard-compact.png"));
    const current = await state();
    expect(current.phase).toBe("play");
    expect(current.view.width).toBeGreaterThan(300);
    expect(errors).toEqual([]);
});
