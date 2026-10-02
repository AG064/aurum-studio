import { test, expect } from "@playwright/test";
import { mkdtemp, cp, readFile, writeFile, readdir, mkdir, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { monitorHttp } from "./http-diagnostics.js";
import { openWorkspace } from "./workspace-ready.js";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
let work, project, child, output = "", origin, token, endpoint, page, context;
let httpDiagnostics;
const control = async (path, body) => {
    const response = await fetch(origin + path, { method: body ? "POST" : "GET", headers: { "Content-Type": "application/json", "X-Aurum-Token": token }, ...(body ? { body: JSON.stringify(body) } : {}) });
    const result = await response.json();
    if (!response.ok || !result.ok) throw new Error(result.error || JSON.stringify(result));
    return result;
};
const inspect = async () => {
    await page.locator("#inspect-runtime").click();
    await expect(page.locator("#runtime-status")).toContainText("editable values");
};
// Private exports have bounded native setup/import/export stages. Keep the test
// budget longer than one native stage, rather than aborting a valid in-flight export.
test.describe.configure({ mode: "serial", timeout: 360000 });
test.beforeAll(async ({ browser }) => {
    for (const key of ["AURUM_BINARY", "AURUM_GODOT", "AURUM_WEB_TEMPLATE"]) if (!process.env[key]) throw new Error(`Required integration prerequisite: ${key}`);
    work = await mkdtemp(join(tmpdir(), "aurum-integration-browser-"));
    project = join(work, "live-preview");
    await cp(join(repo, "scripts/tests/fixtures/live-preview"), project, { recursive: true, filter: (path) => ![".aurum", ".godot", "dist"].includes(basename(path)) });
    child = spawn(resolve(process.env.AURUM_BINARY), ["studio", project, "--no-open"], { windowsHide: true, env: { ...process.env, AURUM_STUDIO_HOME: join(work, "state"), APPDATA: join(work, "userdata"), XDG_DATA_HOME: join(work, "userdata") }, stdio: ["ignore", "pipe", "pipe"] });
    child.stdout.on("data", (data) => output += data);
    child.stderr.on("data", (data) => output += data);
    await expect.poll(() => output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/)?.[0], { timeout: 30000 }).toBeTruthy();
    endpoint = output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/)[0];
    origin = new URL(endpoint).origin; token = new URL(endpoint).searchParams.get("t");
    context = await browser.newContext({ viewport: { width: 1488, height: 1056 } });
    page = await context.newPage();
    httpDiagnostics = monitorHttp(page);
});
test.afterAll(async ({}, info) => {
    if (context) await context.close();
    if (origin && child?.exitCode === null) await control("/api/stop", {}).catch(() => {});
    if (child?.exitCode === null) await Promise.race([new Promise((resolve) => child.once("exit", resolve)), new Promise((resolve) => setTimeout(resolve, 5000))]);
    if (child?.exitCode === null) child.kill();
    await writeFile(info.outputPath("studio.log"), token ? output.replaceAll(token, "[redacted]") : output);
    await writeFile(info.outputPath("evidence.json"), JSON.stringify({work,project},null,2));
    if (httpDiagnostics) await writeFile(info.outputPath("http-diagnostics.json"), JSON.stringify(httpDiagnostics(), null, 2));
    // Keep completed native-stage logs even if the browser fails while a new
    // export is pending. Only this disposable fixture's logs are collected.
    let logCount = 0;
    const collect = async (directory, relative = "", depth = 0) => {
        if (depth > 8 || logCount >= 64) return;
        for (const entry of await readdir(directory, { withFileTypes: true }).catch(() => [])) {
            const path = join(directory, entry.name), name = join(relative, entry.name);
            if (entry.isDirectory()) await collect(path, name, depth + 1);
            else if (entry.isFile() && entry.name.endsWith(".log") && logCount < 64) {
                const metadata = await stat(path).catch(() => null);
                if (!metadata || metadata.size > 2 * 1024 * 1024) continue;
                const text = await readFile(path, "utf8").catch(() => null);
                if (text === null) continue;
                const target = info.outputPath("native-logs", name);
                await mkdir(dirname(target), { recursive: true });
                await writeFile(target, token ? text.replaceAll(token, "[redacted]") : text);
                logCount++;
            }
        }
    };
    if (project) await collect(join(project, ".aurum"));
});
test("ordinary projects preserve export filters, HTML options and notices", async ({}, info) => {
    // Cold native import/export and visible runtime checks share this bounded budget.
    test.setTimeout(300000);
    await openWorkspace(page, endpoint);
    await expect(page.locator("#project")).toHaveText("live-preview");
    await expect(page.locator("#workspace-status")).toHaveText("Load workspace complete");
    await page.locator("#run-web").click();
    await expect(page.locator("#preview-state")).toHaveText("Preview ready", { timeout: 120000 });
    if (!(await page.locator("#runtime-inspector summary").isVisible())) await page.locator("#toggle-inspector").click();
    await page.locator("#runtime-inspector summary").click();
    await page.locator("#auto-rebuild").uncheck();
    await inspect();
    await expect(page.getByLabel("Live config_present", { exact: true })).toHaveValue("true");
    await expect(page.getByLabel("Live notice_present", { exact: true })).toHaveValue("true");
    await expect(page.getByLabel("Live private_present", { exact: true })).toHaveValue("false");
    const url = await page.locator("#preview-frame").getAttribute("src");
    expect(await (await fetch(url)).text()).toContain('name="aurum-preset-fixture"');
    await page.locator("#preview-frame").screenshot({ path: info.outputPath("ordinary-game.png") });
});
test("live edits are acknowledged and invalid types leave the game intact", async () => {
    const input = page.getByLabel("Live counter", { exact: true });
    if (await input.inputValue() !== "187") {
        await input.fill("187"); await input.press("Tab");
        await expect(page.locator("#runtime-status")).toHaveText("counter applied without restarting.");
    }
    await page.getByLabel("Live speed", { exact: true }).fill('"invalid"');
    await page.getByLabel("Live speed", { exact: true }).press("Tab");
    await expect(page.locator("#runtime-status")).toContainText("Property type mismatch");
    await inspect();
    await expect(input).toHaveValue("187");
    await expect(page.getByLabel("Live speed", { exact: true })).toHaveValue("7");
});
test("same-size source edits rebuild on Run and preserve progress while applying new defaults", async () => {
    const before = await page.locator("#preview-frame").getAttribute("src");
    const file = join(project, "godot/main.gd"); const text = await readFile(file, "utf8");
    const changed = text.replace('"Original"', '"Revised!"');
    expect(Buffer.byteLength(changed)).toBe(Buffer.byteLength(text));
    await writeFile(file, changed);
    await expect(page.locator("#preview-revision")).toHaveText("Source changed");
    await page.locator("#run-web").click();
    await expect(page.locator("#reload-status")).toHaveText("Rebuilt and restored the checkpoint.", { timeout: 300000 });
    expect(await page.locator("#preview-frame").getAttribute("src")).not.toBe(before);
    await inspect();
    await expect(page.getByLabel("Live counter", { exact: true })).toHaveValue("187");
    await expect(page.getByLabel("Live caption", { exact: true })).toHaveValue('"Revised!"');
});
test("automatic rebuild reaches the visible preview", async () => {
    await page.locator("#auto-rebuild").check();
    const before = await page.locator("#preview-frame").getAttribute("src");
    const file = join(project, "godot/main.gd");
    await writeFile(file, (await readFile(file, "utf8")).replace('"Revised!"', '"Version3"'));
    await expect.poll(() => page.locator("#preview-frame").getAttribute("src"), { timeout: 300000 }).not.toBe(before);
    await expect(page.locator("#reload-status")).toHaveText("Rebuilt and restored the checkpoint.", { timeout: 300000 });
    await inspect();
    await expect(page.getByLabel("Live caption", { exact: true })).toHaveValue('"Version3"');
    await page.locator("#auto-rebuild").uncheck();
});
test("external API builds reach the visible preview and restore progress", async () => {
    await inspect();
    const input = page.getByLabel("Live counter", { exact: true });
    if (await input.inputValue() !== "187") {
        await input.fill("187"); await input.press("Tab");
        await expect(page.locator("#runtime-status")).toHaveText("counter applied without restarting.");
    }
    // Exercise cold source import alongside a private export repeatedly. Each
    // comment edit invalidates the native source cache without changing gameplay.
    const file = join(project, "godot/main.gd");
    for (let iteration = 0; iteration < 3; iteration++) {
        await writeFile(file, (await readFile(file, "utf8")) + `\n# Native concurrency probe ${iteration}\n`);
        const building = control("/api/preview", { project, force: true });
        const current = await Promise.race([
            control("/api/preview"),
            new Promise((_, reject) => setTimeout(() => reject(new Error("Preview status was blocked by export")), 5000)),
        ]);
        expect(current.url).toBe(await page.locator("#preview-frame").getAttribute("src"));
        await expect(page.locator("#reload-status")).toHaveText("Agent build in progress. Existing run frozen.");
        const scene = await control("/api/project", { project, op: "scene_inspect", scene: "main.tscn" });
        expect(scene.tree.type).toBe("Node2D");
        const external = await building;
        await expect(page.locator("#preview-frame")).toHaveAttribute("src", external.url, { timeout: 120000 });
        await expect(page.locator("#reload-status")).toHaveText("Rebuilt and restored the checkpoint.", { timeout: 120000 });
        await inspect();
        await expect(page.getByLabel("Live counter", { exact: true })).toHaveValue("187");
    }
});
test("invalid builds retain the old preview and its checkpoint", async () => {
    const before = await page.locator("#preview-frame").getAttribute("src");
    const file = join(project, "godot/main.gd"); const valid = await readFile(file, "utf8");
    await writeFile(file, valid + "\nfunc invalid(:\n");
    await page.locator("#run-web").click();
    await expect(page.locator("#preview-state")).toHaveText("Preview retained", { timeout: 120000 });
    expect(await page.locator("#preview-frame").getAttribute("src")).toBe(before);
    await inspect();
    await expect(page.getByLabel("Live counter", { exact: true })).toHaveValue("187");
    await writeFile(file, valid);
});

test("incompatible restored scenes roll back and resume the retained game", async () => {
    const before = await page.locator("#preview-frame").getAttribute("src");
    const settingsFile = join(project, "godot/project.godot");
    const settings = await readFile(settingsFile, "utf8");
    await writeFile(join(project, "godot/other.tscn"), await readFile(join(project, "godot/main.tscn"), "utf8"));
    await writeFile(settingsFile, settings.replace('res://main.tscn', 'res://other.tscn'));
    await control("/api/preview", { project, force: true });
    await expect(page.locator("#reload-status")).toContainText("scene does not match", { timeout: 120000 });
    await expect(page.locator("#preview-state")).toHaveText("Preview retained");
    expect(await page.locator("#preview-frame").getAttribute("src")).toBe(before);
    await inspect();
    await expect(page.getByLabel("Live counter", { exact: true })).toHaveValue("187");
    const active = await control("/api/preview");
    expect(active.url).toBe(before);
    const elapsed = Number(await page.getByLabel("Live elapsed", { exact: true }).inputValue());
    await expect.poll(async () => {
        await inspect();
        return Number(await page.getByLabel("Live elapsed", { exact: true }).inputValue());
    }, { timeout: 10000 }).toBeGreaterThan(elapsed);
    await writeFile(settingsFile, settings);
});

test("stopping during a build prevents late publication", async () => {
    const directory = join(project, ".aurum/web");
    const previous = new Set(await readdir(directory));
    const building = control("/api/preview", { project, force: true }).then(
        value => ({ value }), error => ({ error: error.message }),
    );
    await expect.poll(async () => (await readdir(directory)).some(name => !previous.has(name)), { timeout: 10000 }).toBe(true);
    await control("/api/preview", { action: "stop" });
    const outcome = await building;
    expect(outcome.error).toContain("changed the preview");
    expect((await control("/api/preview")).active).toBe(false);
    await expect(page.locator("#preview-frame")).not.toHaveAttribute("src", /.+/, { timeout: 10000 });
});
