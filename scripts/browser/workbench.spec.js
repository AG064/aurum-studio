import { test, expect } from "@playwright/test";
import { mkdtemp, mkdir, cp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { createServer } from "node:http";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
let child, work, project, endpoint, token, origin, page, context;
let processOutput = "",
    errors = [];
const state = async () =>
    JSON.parse(
        await page.locator("#live-runtime-state").getAttribute("data-snapshot"),
    );
const control = async (path, body, headers = {}) =>
    fetch(origin + path, {
        method: body ? "POST" : "GET",
        headers: {
            "Content-Type": "application/json",
            "X-Aurum-Token": token,
            ...headers,
        },
        ...(body ? { body: JSON.stringify(body) } : {}),
    });

test.describe.configure({ mode: "serial" });
test.beforeAll(async ({ browser }) => {
    if (
        !process.env.AURUM_BINARY ||
        !process.env.AURUM_GODOT ||
        !process.env.AURUM_WEB_TEMPLATE
    ) {
        throw new Error(
            "Supply AURUM_BINARY, AURUM_GODOT and AURUM_WEB_TEMPLATE. Browser acceptance never skips missing prerequisites.",
        );
    }
    work = await mkdtemp(join(tmpdir(), "aurum-browser-"));
    project = join(work, "orbit-break");
    await cp(join(repository, "examples/orbit-break"), project, {
        recursive: true,
        filter: (path) =>
            ![".godot", ".aurum", "dist", ".git"].includes(basename(path)),
    });
    await mkdir(join(work, "userdata"));
    child = spawn(
        resolve(process.env.AURUM_BINARY),
        ["studio", project, "--no-open"],
        {
            windowsHide: true,
            env: {
                ...process.env,
                AURUM_STUDIO_HOME: join(work, "studio-state"),
                APPDATA: join(work, "userdata"),
                XDG_DATA_HOME: join(work, "userdata"),
            },
            stdio: ["ignore", "pipe", "pipe"],
        },
    );
    child.stdout.on("data", (data) => {
        processOutput += data;
    });
    child.stderr.on("data", (data) => {
        processOutput += data;
    });
    child.on("error", (error) => {
        processOutput += error.message;
    });
    await expect
        .poll(
            () =>
                processOutput.match(
                    /http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/,
                )?.[0],
            {
                timeout: 30_000,
                message: "Studio must start with an authenticated loopback URL",
            },
        )
        .toBeTruthy();
    endpoint = processOutput.match(
        /http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/,
    )[0];
    origin = new URL(endpoint).origin;
    token = new URL(endpoint).searchParams.get("t");
    context = await browser.newContext({
        viewport: { width: 1488, height: 1056 },
    });
    page = await context.newPage();
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
        if (message.type() === "error") errors.push(message.text());
    });
});

test.afterEach(async ({}, info) => {
    if (page && info.status !== info.expectedStatus) {
        await page
            .screenshot({
                path: info.outputPath("failure.png"),
                fullPage: true,
            })
            .catch(() => {});
        await info.attach("browser-errors", {
            body: JSON.stringify(errors, null, 2),
            contentType: "application/json",
        });
    }
});

test.afterAll(async ({}, info) => {
    if (context) await context.close();
    if (endpoint && child?.exitCode === null) {
        await control("/api/stop", {}).catch(() => {});
        await Promise.race([
            new Promise((resolve) => child.once("exit", resolve)),
            new Promise((resolve) => setTimeout(resolve, 5000)),
        ]);
    }
    if (child?.exitCode === null) child.kill();
    if (work) {
        const redacted = token
            ? processOutput.replaceAll(token, "[redacted]")
            : processOutput;
        await writeFile(info.outputPath("studio.log"), redacted);
        await writeFile(
            info.outputPath("evidence.json"),
            JSON.stringify({ work, errors }, null, 2),
        );
    }
});

test("real export boots a rendered game on an isolated origin", async ({}, info) => {
    await page.goto(endpoint);
    await expect(page.locator("#project")).toHaveText("orbit-break");
    await expect(page.locator("#workspace-status")).toHaveText(
        "Load workspace complete",
    );
    await expect(page.locator("#file-list")).not.toContainText(".uid");
    await page.getByRole("button", { name: "Run", exact: true }).click();
    await expect
        .poll(async () => (await state())?.phase, { timeout: 120_000 })
        .toBe("menu");
    const src = await page.locator("#preview-frame").getAttribute("src");
    expect(new URL(src).origin).not.toBe(origin);
    expect(src).not.toContain(token);
    const wasm = await fetch(new URL("index.wasm", src));
    expect(wasm.headers.get("content-type")).toBe("application/wasm");
    expect((await wasm.arrayBuffer()).byteLength).toBe(
        Number(wasm.headers.get("content-length")),
    );
    await expect(
        page.frameLocator("#preview-frame").locator("canvas"),
    ).toBeVisible();
    await page.screenshot({ path: info.outputPath("workbench-menu.png") });
    await page
        .locator("#preview-frame")
        .screenshot({ path: info.outputPath("game-menu.png") });
    expect(errors).toEqual([]);
});

test("keyboard play and acknowledged live edits preserve the same run", async ({}, info) => {
    test.setTimeout(300000);
    const canvas = page.frameLocator("#preview-frame").locator("canvas");
    const menu = await state();
    await canvas.press("Enter");
    await expect.poll(async () => (await state()).phase).toBe("hangar");
    await canvas.press("Digit1");
    await page.keyboard.press("Space");
    // Pause before inspecting state: slow software-rendered CI can spend seconds
    // on each locator/trace snapshot while the actual game keeps progressing.
    await page.keyboard.press("Escape");
    await expect.poll(async () => (await state()).phase).toBe("paused");
    const beforeMove = await state();
    expect(beforeMove.wave).toBe(1);
    // A completed action is durable; its short cooldown may expire between
    // browser-driver calls on a loaded software-rendering runner.
    expect(beforeMove.dashes).toBeGreaterThan(menu.dashes);
    await page.keyboard.press("Escape");
    await page.keyboard.down("d");
    await expect
        .poll(async () => (await state()).x)
        .toBeGreaterThan(beforeMove.x + 1);
    await page.keyboard.up("d");
    await page.keyboard.press("Escape");
    await expect
        .poll(async () => (await state()).phase)
        .toMatch(/^(paused|upgrade)$/);
    const before = await state();
    const session = await page.locator("#preview-frame").getAttribute("src");
    const speed = page.getByLabel("Live player speed");
    await speed.press("Home");
    await speed.press("ArrowRight");
    await expect(speed).toHaveValue("3.1");
    // Preserve a real keyboard check, then batch the remaining native range steps.
    // This drives the UI input event, not the game state or a hidden test endpoint.
    await speed.evaluate(input => {
        input.stepUp(59);
        input.dispatchEvent(new Event("input", { bubbles: true }));
        input.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await page.getByLabel("Live damage multiplier").fill("1.75");
    await page.getByLabel("Live spawn interval").fill("0.7");
    await expect(page.locator("#live-status")).toHaveText(
        "Applied to the running game. Run state preserved.",
    );
    await expect.poll(async () => (await state()).speed).toBe(9);
    await expect.poll(async () => (await state()).damage_multiplier).toBe(1.75);
    const after = await state();
    expect(after.time).toBe(before.time);
    expect(after.wave).toBe(before.wave);
    expect(after.health).toBe(before.health);
    expect(after.x).toBe(before.x);
    expect(after.hot_reloads).toBeGreaterThan(before.hot_reloads);
    expect(await page.locator("#preview-frame").getAttribute("src")).toBe(
        session,
    );
    if (before.phase === "paused") {
        await canvas.press("Escape");
        await page.screenshot({
            path: info.outputPath("workbench-playing.png"),
        });
        await page
            .locator("#preview-frame")
            .screenshot({ path: info.outputPath("gameplay-polished.png") });
    }
    // Normal simulation, no state injection or forced wins. The first wave opens the workshop.
    await expect
        .poll(async () => (await state()).phase, { timeout: 60_000 })
        .toBe("upgrade");
    const workshop = await state();
    expect(workshop.choices).toEqual(["evolve", "lance", "arc"]);
    expect(workshop.upgrades).toBe(0);
    await page.screenshot({ path: info.outputPath("workshop.png") });
    expect((await state()).time).toBe(workshop.time);
    await page.setViewportSize({ width: 390, height: 844 });
    await expect
        .poll(async () => {
            const current = await state();
            return current.view_height > current.view_width;
        })
        .toBe(true);
    await page
        .locator("#preview-frame")
        .screenshot({ path: info.outputPath("workshop-portrait.png") });
    expect((await state()).choices).toHaveLength(3);
    await page.getByRole("button", { name: "Full-screen preview" }).click();
    await expect
        .poll(async () => (await page.locator("#preview-frame").boundingBox())?.height)
        .toBeGreaterThan(620);
    await page
        .locator("#preview-frame")
        .screenshot({ path: info.outputPath("workshop-portrait-fullscreen.png") });
    // Browser chrome does not receive synthetic Escape as a trusted fullscreen exit.
    // Use the browser API for layout cleanup, without changing game state.
    await page.evaluate(() => document.exitFullscreen());
    await expect
        .poll(async () => (await page.locator("#preview-frame").boundingBox())?.height)
        .toBeLessThan(620);
    expect((await state()).phase).toBe("upgrade");
    expect((await state()).time).toBe(workshop.time);
    await page.setViewportSize({ width: 1024, height: 720 });
    await expect
        .poll(async () => {
            const current = await state();
            const frame = await page.locator("#preview-frame").boundingBox();
            // Godot's stretched design viewport is not the physical iframe size.
            return current.view_width > current.view_height && frame?.height < 520;
        })
        .toBe(true);
    await page
        .locator("#preview-frame")
        .screenshot({ path: info.outputPath("workshop-compact.png") });
    await page.setViewportSize({ width: 1488, height: 1056 });
    await expect
        .poll(async () => {
            const current = await state();
            return current.view_width > current.view_height;
        })
        .toBe(true);
    await canvas.press("Digit2");
    await page.keyboard.press("Escape");
    await expect.poll(async () => (await state()).wave).toBe(2);
    await expect.poll(async () => (await state()).weapon).toBe("LANCE");
    expect((await state()).upgrades).toBe(1);
    expect((await state()).choices).toEqual([]);
    await expect.poll(async () => (await state()).phase).toBe("paused");
    await page.setViewportSize({ width: 390, height: 844 });
    await expect
        .poll(async () => {
            const current = await state();
            return current.view_height > current.view_width;
        })
        .toBe(true);
    await canvas.press("Escape");
    await expect.poll(async () => (await state()).phase).toBe("playing");
    const narrowStart = (await state()).time;
    await expect.poll(async () => (await state()).time).toBeGreaterThan(narrowStart + 1);
    await page
        .locator("#preview-frame")
        .screenshot({ path: info.outputPath("instruments-portrait.png") });
    await page.keyboard.press("Escape");
    await expect.poll(async () => (await state()).phase).toBe("paused");
    await page.setViewportSize({ width: 1488, height: 1056 });
    await page.screenshot({ path: info.outputPath("workbench-wave-two.png") });
    await page
        .locator("#preview-frame")
        .screenshot({ path: info.outputPath("game-wave-two.png") });
    expect(errors).toEqual([]);
});

test("source save and undo work; invalid rebuild retains the running preview", async () => {
    const original = await readFile(join(project, "godot/main.gd"), "utf8");
    await page.getByRole("button", { name: "main.gd", exact: true }).click();
    const editor = page.getByRole("textbox", {
        name: "Source editor",
        exact: true,
    });
    await expect(editor).toHaveValue(original);
    await editor.fill(original + "\n# Browser acceptance edit\n");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.locator("#workspace-status")).toHaveText(
        "Save file complete",
    );
    await expect
        .poll(() => readFile(join(project, "godot/main.gd"), "utf8"))
        .toContain("# Browser acceptance edit");
    await page.getByRole("button", { name: "Undo save" }).click();
    await expect
        .poll(() => readFile(join(project, "godot/main.gd"), "utf8"))
        .toBe(original);
    const session = await page.locator("#preview-frame").getAttribute("src");
    const file = await (
        await control("/api/project", {
            project,
            op: "read",
            path: "godot/main.gd",
        })
    ).json();
    const broken = await control("/api/project", {
        project,
        op: "write",
        path: "godot/main.gd",
        text: "this is not GDScript\n",
        expected_sha256: file.sha256,
    });
    expect(broken.ok).toBeTruthy();
    try {
        const failed = await control("/api/preview", { project, force: true });
        expect(failed.ok).toBeFalsy();
        expect(await page.locator("#preview-frame").getAttribute("src")).toBe(
            session,
        );
        const retained = await (
            await control("/api/preview")
        ).json();
        expect(retained.url).toBe(session);
        expect(retained.stale).toBe(true);
        expect((await state()).phase).toBe("paused");
    } finally {
        const changed = await (
            await control("/api/project", {
                project,
                op: "read",
                path: "godot/main.gd",
            })
        ).json();
        const restored = await control("/api/project", {
            project,
            op: "undo",
            path: "godot/main.gd",
            expected_sha256: changed.sha256,
        });
        expect(restored.ok).toBeTruthy();
    }
});

test("the game cannot operate Studio through shared loopback cookies or forged messages", async () => {
    const preview = new URL(
        await page.locator("#preview-frame").getAttribute("src"),
    );
    const cookies = await context.cookies(origin);
    const response = await fetch(origin + "/api/command", {
        method: "POST",
        headers: {
            "Content-Type": "application/json",
            Origin: preview.origin,
            Cookie: cookies
                .map((cookie) => `${cookie.name}=${cookie.value}`)
                .join("; "),
        },
        body: JSON.stringify({ command: "stop" }),
    });
    expect(response.status).toBe(403);
    expect((await fetch(origin + "/api/state")).status).toBe(401);
    expect(
        (await fetch(new URL("live.json", preview), { method: "POST" })).status,
    ).toBe(405);
    const frame = page.frames().find((frame) => frame.url() === preview.href);
    await frame.evaluate(
        (parentOrigin) =>
            parent.postMessage(
                {
                    type: "aurum-preview-state",
                    session: "forged",
                    state: { phase: "pwned" },
                },
                parentOrigin,
            ),
        origin,
    );
    await expect(page.locator("#live-runtime-state")).not.toContainText(
        "pwned",
    );
});

test("responsive inspector and keyboard file navigation remain usable", async ({}, info) => {
    await page.getByRole("button", { name: "Scene", exact: true }).click();
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(
        page.getByRole("button", { name: "Inspector", exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Inspector", exact: true }).click();
    await expect(page.getByLabel("Live player speed")).toBeVisible();
    expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
    ).toBeLessThanOrEqual(390);
    await page.screenshot({
        path: info.outputPath("mobile-inspector.png"),
        fullPage: true,
    });
    await page.getByRole("button", { name: "Inspector", exact: true }).click();
    await page.setViewportSize({ width: 1488, height: 1056 });
    await page.getByRole("button", { name: "Files", exact: true }).focus();
    await page.keyboard.press("Enter");
    await expect(
        page.getByRole("textbox", { name: "Source editor", exact: true }),
    ).toBeVisible();
    expect(errors).toEqual([]);
});

test("browser export produces a portable bundle without the Studio bridge", async ({
    browser,
}, info) => {
    await page.goto(endpoint);
    await expect(page.locator("#workspace-status")).toHaveText(
        "Load workspace complete",
    );
    await page
        .locator(".header-actions")
        .getByRole("button", { name: "Export", exact: true })
        .click();
    await page.getByRole("button", { name: "Export browser game" }).click();
    await expect(page.locator("#workspace-status")).toHaveText(
        "Export browser game complete",
        { timeout: 120_000 },
    );
    const html = await readFile(join(project, "dist/web/index.html"), "utf8");
    expect(html).not.toContain("aurum-preview.js");
    expect(html).not.toContain(token);
    expect(
        await readFile(join(project, "dist/web/LICENSE-Godot.txt"), "utf8"),
    ).toContain("Godot");
    const notices = JSON.parse(
        await readFile(
            join(project, "dist/web/third-party-licenses.json"),
            "utf8",
        ),
    );
    expect(Object.keys(notices.licenses).length).toBeGreaterThan(0);
    expect(
        (await readFile(join(project, "dist/web/index.wasm"))).length,
    ).toBeGreaterThan(1_000_000);
    const duplicate = await control("/api/preview", {
        action: "export",
        project,
        output: "dist/web",
    });
    expect(duplicate.ok).toBeFalsy();
    expect(await readFile(join(project, "dist/web/index.html"), "utf8")).toBe(
        html,
    );
    const staticServer = createServer(async (request, response) => {
        try {
            const path =
                decodeURIComponent(
                    new URL(request.url, "http://localhost").pathname,
                ).slice(1) || "index.html";
            if (
                path !== basename(path) ||
                !["GET", "HEAD"].includes(request.method)
            ) {
                response.writeHead(404).end();
                return;
            }
            const bytes = await readFile(join(project, "dist/web", path));
            const type = path.endsWith(".wasm")
                ? "application/wasm"
                : path.endsWith(".js")
                  ? "text/javascript"
                  : path.endsWith(".html")
                    ? "text/html"
                    : "application/octet-stream";
            response
                .writeHead(200, {
                    "Content-Type": type,
                    "Content-Length": bytes.length,
                })
                .end(request.method === "HEAD" ? undefined : bytes);
        } catch {
            response.writeHead(404).end();
        }
    });
    await new Promise((resolve) =>
        staticServer.listen(0, "127.0.0.1", resolve),
    );
    const captureVideo = Boolean(process.env.AURUM_CAPTURE_VIDEO);
    const playerContext = await browser.newContext({
        viewport: { width: 1280, height: 800 },
        ...(captureVideo
            ? {
                  recordVideo: {
                      dir: info.outputPath("video"),
                      size: { width: 1280, height: 800 },
                  },
              }
            : {}),
    });
    const standalone = await playerContext.newPage();
    const standaloneErrors = [];
    standalone.on("pageerror", (error) => standaloneErrors.push(error.message));
    try {
        await standalone.goto(
            `http://127.0.0.1:${staticServer.address().port}/`,
        );
        await expect
            .poll(
                () =>
                    standalone.evaluate(() => {
                        try {
                            return JSON.parse(window.aurumState).phase;
                        } catch {
                            return null;
                        }
                    }),
                { timeout: 60000 },
            )
            .toBe("menu");
        await standalone.locator("canvas").press("Enter");
        await expect
            .poll(() => standalone.evaluate(() => JSON.parse(window.aurumState).phase))
            .toBe("hangar");
        await standalone.locator("canvas").press("Digit1");
        await expect
            .poll(() =>
                standalone.evaluate(() => JSON.parse(window.aurumState).phase),
            )
            .toBe("playing");
        if (captureVideo) {
            for (const key of ["d", "s", "a", "w"]) {
                await standalone.keyboard.down(key);
                await standalone.keyboard.press("Space");
                await standalone.waitForTimeout(900);
                await standalone.keyboard.up(key);
            }
            await standalone.screenshot({
                path: info.outputPath("pulse-combat.png"),
            });
            await expect
                .poll(
                    () =>
                        standalone.evaluate(
                            () => JSON.parse(window.aurumState).phase,
                        ),
                    { timeout: 60000 },
                )
                .toBe("upgrade");
            await standalone.screenshot({
                path: info.outputPath("three-choices.png"),
            });
            await standalone.locator("canvas").press("Digit3");
            for (const key of ["d", "s", "a", "w", "d", "s", "a", "w"]) {
                await standalone.keyboard.down(key);
                await standalone.keyboard.press("Space");
                await standalone.waitForTimeout(900);
                await standalone.keyboard.up(key);
            }
            await standalone.screenshot({
                path: info.outputPath("arc-combat.png"),
            });
        }
        expect(standaloneErrors).toEqual([]);
    } finally {
        const recording = standalone.video();
        await playerContext.close();
        if (recording)
            await info.attach("combat-video", {
                path: await recording.path(),
                contentType: "video/webm",
            });
        await new Promise((resolve) => staticServer.close(resolve));
    }
});

test("an ordinary 2D project boots without the example-specific live bridge", async () => {
    const ordinary = join(work, "plain-2d");
    const created = await control("/api/projects", {
        action: "create",
        path: ordinary,
        name: "plain-demo",
        template: "2d",
    });
    expect(created.ok).toBeTruthy();
    await page.goto(endpoint);
    await expect(page.locator("#workspace-status")).toHaveText(
        "Load workspace complete",
    );
    await page.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.locator("#preview-state")).toHaveText("Preview ready", {
        timeout: 120000,
    });
    await expect(
        page.frameLocator("#preview-frame").locator("canvas"),
    ).toBeVisible();
    await expect(page.locator("#live-tuning")).toBeHidden();
    const runningUrl = await page.locator("#preview-frame").getAttribute("src");
    await page.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.locator("#workspace-status")).toHaveText(
        "Web preview complete",
    );
    await expect(page.locator("#preview-state")).toHaveText("Preview ready");
    expect(await page.locator("#preview-frame").getAttribute("src")).toBe(
        runningUrl,
    );
    expect(errors).toEqual([]);
});
