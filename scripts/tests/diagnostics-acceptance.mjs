// Real CLI, HTTP and stdio MCP diagnostics, using disposable project/state.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, writeFile, readdir, stat, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { performance } from "node:perf_hooks";

const executable = resolve(process.argv[2] || "target/debug/aurum.exe");
const root = await mkdtemp(join(tmpdir(), "aurum-diagnostics-acceptance-"));
const project = join(root, "project");
await mkdir(project);
await writeFile(join(project, "aurum.toml"), 'schema_version = 1\nname = "diagnostics-fixture"\nmodules = []\n');
const environment = { ...process.env, AURUM_STUDIO_HOME: join(root, "state") };
let checks = 0;
const check = (condition, message) => { assert.ok(condition, message); checks++; };
const cli = (input, readOnly = false) => {
    const result = spawnSync(executable, ["project", project, "--request-json", JSON.stringify(input), ...(readOnly ? ["--read-only"] : [])], {
        encoding: "utf8", env: environment, timeout: 15_000, windowsHide: true,
    });
    assert.ifError(result.error);
    return { code: result.status, value: JSON.parse(result.stdout.trim()), stderr: result.stderr };
};
const once = (child) => new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolve(code));
});
const deadline = (promise, ms, label) => new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms} ms`)), ms);
    promise.then(value => { clearTimeout(timer); resolve(value); }, error => { clearTimeout(timer); reject(error); });
});

check(cli({ op: "logs" }, true).value.records.length === 0, "empty read-only tail");
check(!(await readdir(project)).includes(".aurum"), "queries do not create diagnostic state");
check(cli({ op: "write", path: "denied.gd", text: "secret", expected_sha256: "" }, true).code !== 0, "read-only write refused");
check(!(await readdir(project)).includes(".aurum"), "permission refusal creates no state");
const changeCheck = cli({ op: "changes_check", changes: [{ path: "planned.gd", action: "create", expected_sha256: "", text: "planned" }] }, true);
check(changeCheck.code === 0 && changeCheck.value.ready && !changeCheck.value.applied && !changeCheck.value.validated, "read-only change preflight");
check(!(await readdir(project)).includes(".aurum") && !(await readdir(project)).includes("planned.gd"), "preflight creates no files/state");
const payload = "private-source-payload-0123456789";
check(cli({ op: "write", path: "main.gd", text: payload, expected_sha256: "" }).code === 0, "guarded CLI save");
check(cli({ op: "write", path: "main.gd", text: "lost", expected_sha256: "stale" }).code !== 0, "conflict refused");
const tail = cli({ op: "logs", failures_only: true }).value;
const staleCheck = cli({ op: "changes_check", changes: [{ path: "main.gd", action: "replace", expected_sha256: "0".repeat(64), text: "lost" }] }, true);
check(staleCheck.code === 0 && !staleCheck.value.ready && staleCheck.value.conflicts[0].reason === "stale_hash", "preflight reports stale conflicts as data");
check(tail.records.some(record => record.outcome === "conflict"), "failure category retained");
check(!JSON.stringify(tail).includes(payload), "source omitted from query");

const mcp = spawn(executable, ["mcp", "--root", project, "--tools", "studio", "--trace"], { env: environment, windowsHide: true });
const mcpDone = once(mcp);
let mcpOutput = "", mcpTrace = "";
mcp.stdout.on("data", data => { mcpOutput += data; });
mcp.stderr.on("data", data => { mcpTrace += data; });
for (const request of [
    { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", clientInfo: { name: "fixture", version: "1" }, capabilities: {} } },
    { jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "aurum_project_action", arguments: { op: "write", path: "from-mcp.gd", text: payload, expected_sha256: "" } } },
    { jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "aurum_project_query", arguments: { op: "logs", limit: 3 } } },
    { jsonrpc: "2.0", id: 4, method: "tools/call", params: { name: "aurum_project_query", arguments: { op: "changes_check", changes: [{ path: "planned-mcp.gd", action: "create", expected_sha256: "", text: "planned" }] } } },
]) mcp.stdin.write(`${JSON.stringify(request)}\n`);
mcp.stdin.end();
check(await deadline(mcpDone, 15_000, "MCP") === 0, "MCP exit");
const responses = mcpOutput.trim().split("\n").map(line => JSON.parse(line));
check(responses.length === 4 && responses.every(response => response.jsonrpc === "2.0"), "MCP stdout contains protocol only");
check(responses[2].result.structuredContent.records.length <= 3, "bounded structured MCP tail");
check(responses[3].result.structuredContent.ready && !responses[3].result.structuredContent.applied, "MCP change preflight");
check(mcpTrace.includes("payload=omitted") && !mcpTrace.includes(payload), "payload-free real protocol trace");

// A real native process, not a game-playability test. It prints after the
// launching CLI has exited, exercising the detached relay lifetime contract.
const nativeSource = join(root, "native-fixture.rs");
const nativeExecutable = join(root, process.platform === "win32" ? "native-fixture.exe" : "native-fixture");
await writeFile(nativeSource, `#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() {
    if std::env::args().any(|arg| arg == "--version") { println!("4.7.stable.fixture"); return; }
    let args: Vec<_> = std::env::args().collect();
    let project = args.windows(2).find(|pair| pair[0] == "--path").map(|pair| pair[1].clone()).unwrap();
    let trigger = std::path::Path::new(&project).join("continue-native");
    let started = std::time::Instant::now();
    while !trigger.exists() && started.elapsed().as_secs() < 30 { std::thread::sleep(std::time::Duration::from_millis(20)); }
    println!("relay-after-cli-exit-59");
    std::thread::sleep(std::time::Duration::from_secs(2));
}
`);
const compile = spawnSync("rustc", ["--crate-name", "native_fixture", nativeSource, "-o", nativeExecutable], { env: environment, encoding: "utf8", windowsHide: true, timeout: 30_000 });
assert.ifError(compile.error);
assert.equal(compile.status, 0, compile.stderr);
await writeFile(join(project, "project.godot"), '[application]\nconfig/name="Diagnostics fixture"\n');
const launch = spawnSync(executable, ["run", project, "--godot", nativeExecutable, "--json"], { env: environment, encoding: "utf8", windowsHide: true, timeout: 15_000 });
assert.ifError(launch.error);
assert.equal(launch.status, 0, launch.stderr);
const native = JSON.parse(launch.stdout);
try {
    await writeFile(join(project, "continue-native"), "ready");
    const until = performance.now() + 10_000;
    while (!(await readFile(native.log, "utf8")).includes("relay-after-cli-exit-59")) {
        assert.ok(performance.now() < until, "detached relay lost output after CLI exit");
        await new Promise(resolve => setTimeout(resolve, 50));
    }
    check(true, "native output survives launching CLI exit");
} finally {
    spawnSync(executable, ["stop", project, "--force", "--json"], { env: environment, encoding: "utf8", windowsHide: true, timeout: 15_000 });
}

const linkedProject = join(root, "linked-project"), outside = join(root, "outside-state");
await mkdir(linkedProject); await mkdir(outside);
await writeFile(join(linkedProject, "aurum.toml"), 'schema_version=1\nname="linked-fixture"\nmodules=[]\n');
await writeFile(join(outside, "keep.txt"), "unchanged");
await symlink(outside, join(linkedProject, ".aurum"), process.platform === "win32" ? "junction" : "dir");
const linked = spawnSync(executable, ["project", linkedProject, "--request-json", '{"op":"logs"}'], { env: environment, encoding: "utf8", windowsHide: true, timeout: 15_000 });
check(linked.status !== 0, "linked diagnostic state refused");
check((await readdir(outside)).length === 1 && await readFile(join(outside, "keep.txt"), "utf8") === "unchanged", "outside state untouched");

const server = spawn(executable, ["studio", project, "--json", "--port", "0"], { env: environment, windowsHide: true });
const serverDone = once(server);
let serverOutput = "";
let address;
try {
    address = await deadline(new Promise((resolve, reject) => {
        server.once("error", reject);
        server.stdout.on("data", data => {
            serverOutput += data;
            const line = serverOutput.split("\n").find(line => line.startsWith("{"));
            if (line) { try { resolve(JSON.parse(line)); } catch {} }
        });
    }), 15_000, "HTTP startup");
    const url = new URL(address.url);
    const token = url.searchParams.get("t");
    const request = async input => {
        const response = await fetch(`${url.origin}/api/project`, {
            method: "POST", headers: { "X-Aurum-Token": token, "Content-Type": "application/json" },
            body: JSON.stringify(input), signal: AbortSignal.timeout(15_000),
        });
        return { code: response.status, value: await response.json() };
    };
    const plannedHttp = await request({ op: "changes_check", changes: [{ path: "planned-http.gd", action: "create", expected_sha256: "", text: "planned" }] });
    check(plannedHttp.code === 200 && plannedHttp.value.ready && !plannedHttp.value.applied, "HTTP change preflight");
    check((await request({ op: "write", path: "from-http.gd", text: payload, expected_sha256: "" })).code === 200, "HTTP guarded save");
    check((await request({ op: "logs", limit: 4 })).value.records.length <= 4, "HTTP bounded tail");
    const begin = performance.now();
    for (let index = 0; index < 30; index++) {
        const result = await request({ op: "write", path: `timing-${index}.gd`, text: "fixture", expected_sha256: "" });
        assert.equal(result.code, 200);
    }
    const elapsed = performance.now() - begin;
    const raw = await readFile(join(project, ".aurum/logs/operations.jsonl"), "utf8");
    check(!raw.includes(payload) && !raw.includes(token), "on-disk source/token omission");
    const records = raw.trim().split("\n").map(line => JSON.parse(line));
    check(records.every(record => ["started", "completed"].includes(record.phase)), "valid operation record phases");
    check(records.filter(record => record.phase === "completed").length >= 34, "all mutating paths use shared journal");
    const logBytes = (await stat(join(project, ".aurum/logs/operations.jsonl"))).size;
    await writeFile(join(root, "verification.json"), JSON.stringify({ checks, root, logBytes,
        httpThirtyWritesMs: elapsed, httpMeanWriteMs: elapsed / 30,
        note: "End-to-end local HTTP save latency, not isolated logger cost or model token savings" }, null, 2));
} finally {
    if (address) {
        const url = new URL(address.url);
        try { await fetch(`${url.origin}/api/stop`, { method: "POST", headers: { "X-Aurum-Token": url.searchParams.get("t") }, signal: AbortSignal.timeout(5_000) }); } catch {}
    }
    try { await deadline(serverDone, 10_000, "server shutdown"); } catch { server.kill(); await deadline(serverDone, 5_000, "server termination"); }
}
console.log(JSON.stringify({ checks, evidence: root, status: "passed" }));
