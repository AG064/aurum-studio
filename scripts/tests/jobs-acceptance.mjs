// Real compiled CLI, HTTP and MCP background jobs with owned Godot workers.
import assert from "node:assert/strict";
import {spawn, spawnSync} from "node:child_process";
import {mkdtemp, mkdir, writeFile, readFile, readdir, stat, lstat, realpath} from "node:fs/promises";
import {createHash} from "node:crypto";
import {tmpdir} from "node:os";
import {join, resolve} from "node:path";

const [binary, godot] = process.argv.slice(2);
assert.ok(binary && godot, "Usage: jobs-acceptance.mjs <aurum> <godot>");
assert.equal(process.platform, "win32", "This fixture qualifies Windows owned-worker cancellation");
const root = await mkdtemp(join(tmpdir(), "aurum-jobs-")), project = join(root, "project");
const environment = {...process.env, AURUM_GODOT: resolve(godot), AURUM_STUDIO_HOME: join(root, "state"), APPDATA: join(root, "user"), LOCALAPPDATA: join(root, "user")};
const hash = text => createHash("sha256").update(text).digest("hex");
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const bound = (promise, ms, label) => new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} deadline exceeded`)), ms);
    promise.then(value => { clearTimeout(timer); resolve(value); }, error => { clearTimeout(timer); reject(error); });
});
const exists = async path => { try { await stat(path); return true; } catch (error) { if (error.code === "ENOENT") return false; throw error; } };
const until = async (probe, accept, label, ms = 15000) => {
    const deadline = Date.now() + ms;
    while (Date.now() < deadline) { const value = await probe(); if (accept(value)) return value; await delay(50); }
    throw new Error(`${label} deadline exceeded`);
};
const run = (args, ms = 30000) => new Promise((resolve, reject) => {
    const child = spawn(resolveBinary, args, {env: environment, windowsHide: true});
    let stdout = "", stderr = "";
    child.stdout.on("data", data => { stdout += data; if (Buffer.byteLength(stdout) > 4 * 1024 * 1024) { child.kill(); reject(new Error("CLI output bound exceeded")); } });
    child.stderr.on("data", data => { stderr = (stderr + data).slice(-65536); });
    child.once("error", reject);
    const timer = setTimeout(() => { child.kill(); reject(new Error("CLI deadline exceeded")); }, ms);
    child.once("close", code => { clearTimeout(timer); let value; try { value = JSON.parse(stdout); } catch { value = {error: stderr}; } resolve({code, value}); });
});
const resolveBinary = resolve(binary);
const cli = (input, readOnly = false) => run(["project", project, "--request-json", JSON.stringify(input), ...(readOnly ? ["--read-only"] : [])]);
const inspectProcess = pid => {
    assert.ok(Number.isInteger(pid) && pid > 0, "A witnessed process has a valid PID");
    const command = `$jobProcess = Get-CimInstance -ClassName Win32_Process -Filter 'ProcessId = ${pid}' -ErrorAction Stop; if ($null -eq $jobProcess) { 'null'; exit 0 }; if (-not $jobProcess.ExecutablePath -or -not $jobProcess.CreationDate) { throw 'Process identity is inaccessible' }; [ordered]@{pid=[int]$jobProcess.ProcessId;path=[string]$jobProcess.ExecutablePath;created=$jobProcess.CreationDate.ToUniversalTime().Ticks.ToString()} | ConvertTo-Json -Compress; exit 0`;
    const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", command], {windowsHide: true, encoding: "utf8", timeout: 10000});
    assert.ifError(result.error); assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim() ? JSON.parse(result.stdout) : null;
};
const originalGone = identity => { const current = inspectProcess(identity.pid); return !current || current.created !== identity.created; };
const main = `extends Node3D
@export var counter: int = 7
func _ready():
\tprocess_mode = Node.PROCESS_MODE_ALWAYS
func _input(event):
\tif event.is_action_pressed("pause"): get_tree().paused = not get_tree().paused
\tif event.is_action_pressed("advance") and not get_tree().paused: counter += 1
func aurum_capture_state():
\treturn {"schema": 1, "counter": counter, "position": position}
func aurum_restore_state(state):
\tif not state is Dictionary or state.get("schema") != 1 or not state.get("position") is Vector3: return false
\tvar value = state.get("counter")
\tif not typeof(value) in [TYPE_INT, TYPE_FLOAT] or not is_finite(float(value)) or value != floor(value): return false
\tcounter = int(value)
\tposition = state.position
\treturn true
`;
const sourceSentinel = "private-job-source-sentinel", smallText = "private-result-sentinel\n";
const blocker = marker => `@tool\nextends Resource\n# ${sourceSentinel}\nstatic func _static_init():\n\tvar file = FileAccess.open(${JSON.stringify(marker.replaceAll("\\", "/"))}, FileAccess.WRITE)\n\tif file == null: return\n\tfile.store_string(JSON.stringify({"pid": OS.get_process_id()}))\n\tfile.close()\n\tOS.delay_msec(120000)\n`;
const holdRequest = name => ({op: "changes_validate", changes: [{path: `jobs/${name}.gd`, action: "create", expected_sha256: "", text: blocker(join(root, `${name}.json`))}]});
const marker = name => until(async () => { const path = join(root, `${name}.json`); return await exists(path) ? JSON.parse(await readFile(path, "utf8")) : null; }, value => value?.pid > 0, `${name} owned worker marker`);
let checks = 0, owner, host, token, server, serverDone, witness, witnessIdentity, stderr = "";
const clients = [], submitted = [];
const evidence = {scope: "compiled background-job contracts and controlled Windows cancellation; no model-token or task-success benchmark"};
const check = (condition, message) => { assert.ok(condition, message); checks++; };
const owned = (op, job, extra = {}) => ({op, owner_capability: owner, ...(job ? {job_id: typeof job === "string" ? job : job.job_id} : {}), ...extra});
const http = async input => {
    const response = await fetch(`http://127.0.0.1:${host.port}/api/project`, {method: "POST", headers: {"Content-Type": "application/json", "X-Aurum-Token": token}, body: JSON.stringify(input), signal: AbortSignal.timeout(330000)});
    const body = await response.text(); let value; try { value = JSON.parse(body); } catch { value = {error: body.slice(0, 1024)}; }
    return {status: response.status, value};
};
const action = async input => { const reply = await http(input); assert.equal(reply.status, 200, JSON.stringify(reply.value)); return reply.value; };
const refreshHost = async () => { host = JSON.parse(await readFile(join(project, ".aurum/native/host.json"), "utf8")); token = (await readFile(join(project, ".aurum/native/host.token"), "utf8")).trim(); };
const submit = async (request, timeoutSeconds = 120) => {
    const ticket = await action({op: "job_submit", request, timeout_seconds: timeoutSeconds, ...(owner ? {owner_capability: owner} : {})});
    check(ticket.ok === true && ticket.submitted === true && /^[a-f0-9]{32}$/.test(ticket.job_id) && /^[a-f0-9]{64}$/.test(ticket.owner_capability) && ticket.deadline_includes_queue === true, "Admission returns bounded job identity, owner capability and queue deadline contract");
    if (owner) assert.ok(ticket.owner_capability === owner, "Submission keeps the existing private owner capability"); else owner = ticket.owner_capability;
    submitted.push(ticket.job_id); return {job_id: ticket.job_id};
};
const status = job => action(owned("job_status", job));
const terminal = job => until(() => status(job), reply => reply.job?.terminal === true, "Terminal job receipt", 20000);
const summariesSafe = async () => {
    const listing = await action(owned("job_list", null, {limit: 64})), encoded = JSON.stringify(listing);
    check(!encoded.includes(owner) && !encoded.includes(token) && !encoded.includes(sourceSentinel) && !encoded.includes(smallText.trim()) && listing.jobs.every(job => !Object.hasOwn(job, "result") && !Object.hasOwn(job, "request") && !Object.hasOwn(job, "owner_sha256") && Buffer.byteLength(JSON.stringify(job)) <= 8192), "Public summaries omit capabilities, request/result source and private owner hashes");
    const directory = join(project, ".aurum/background-jobs"), names = (await readdir(directory)).filter(name => /^job-[a-f0-9]{32}\.json$/.test(name));
    for (const name of names) { const bytes = await readFile(join(directory, name)), text = bytes.toString("utf8"); assert.ok(bytes.length <= 8192 && !text.includes(owner) && !text.includes(token) && !text.includes(sourceSentinel) && !text.includes(smallText.trim())); const value = JSON.parse(text); assert.ok(!Object.hasOwn(value, "request") && !Object.hasOwn(value, "result")); }
    checks++; return listing;
};
function mcp() {
    const child = spawn(resolveBinary, ["mcp", "--root", project, "--tools", "studio"], {env: environment, windowsHide: true});
    let buffer = "", id = 0; const pending = new Map();
    const fail = error => { for (const entry of pending.values()) entry.reject(error); pending.clear(); child.kill(); };
    const done = new Promise(resolve => child.once("close", code => { for (const entry of pending.values()) entry.reject(new Error("MCP closed before its reply")); pending.clear(); resolve(code); })); child.stderr.on("data", () => {}); child.once("error", fail);
    child.stdout.on("data", data => { try { buffer += data; if (Buffer.byteLength(buffer) > 4 * 1024 * 1024) throw new Error("MCP framing bound exceeded"); for (;;) { const end = buffer.indexOf("\n"); if (end < 0) break; const line = buffer.slice(0, end); buffer = buffer.slice(end + 1); if (!line.trim()) continue; const reply = JSON.parse(line), complete = pending.get(reply.id); if (complete) { pending.delete(reply.id); complete.resolve(reply); } } } catch (error) { fail(error); } });
    const rpc = (method, params) => { const current = ++id, result = new Promise((resolve, reject) => pending.set(current, {resolve, reject})); child.stdin.write(`${JSON.stringify({jsonrpc: "2.0", id: current, method, params})}\n`); return bound(result, 30000, "MCP reply"); };
    const client = {child, done, rpc, call: async (input, query = true) => (await rpc("tools/call", {name: query ? "aurum_project_query" : "aurum_project_action", arguments: input})).result};
    clients.push(client); return client;
}

try {
    await mkdir(join(project, "jobs"), {recursive: true}); await mkdir(join(project, ".godot"));
    const files = {"aurum.toml": 'schema_version=1\nname="jobs-fixture"\nmodules=[]\n', "project.godot": 'config_version=5\n[application]\nrun/main_scene="res://main.tscn"\n[input]\nadvance={"deadzone":0.2,"events":[]}\npause={"deadzone":0.2,"events":[]}\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n', "main.gd": main, "main.tscn": '[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://main.gd" id="1"]\n[node name="Mission" type="Node3D"]\nposition=Vector3(3,0,1)\nscript=ExtResource("1")\n', "working.dll": "working DLL sentinel\n", ".godot/cache-sentinel": "working import cache\n", "small.txt": smallText, "large.txt": "L".repeat(96 * 1024), "huge.txt": "H".repeat(2 * 1024 * 1024)};
    for (const [path, text] of Object.entries(files)) await writeFile(join(project, path), text);
    const preserved = async (expectedMain = main) => { for (const [path, text] of Object.entries(files)) assert.equal(hash(await readFile(join(project, path))), hash(path === "main.gd" ? expectedMain : text), `Working content preserved: ${path}`); assert.deepEqual(await readdir(join(project, ".godot")), ["cache-sentinel"]); checks++; };
    witness = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {env: environment, stdio: "ignore", windowsHide: true});
    witnessIdentity = inspectProcess(witness.pid); assert.ok(witnessIdentity); evidence.unrelated_pid = witness.pid;
    server = spawn(resolveBinary, ["studio", project, "--no-open", "--json", "--port", "0"], {env: environment, windowsHide: true});
    serverDone = new Promise(resolve => server.once("close", resolve)); server.stderr.on("data", data => { stderr = (stderr + data).slice(-65536); });
    await bound(new Promise((resolve, reject) => { let output = ""; server.once("error", reject); server.stdout.on("data", data => { output = (output + data).slice(-65536); for (const line of output.split("\n")) if (line.startsWith("{")) { try { resolve(JSON.parse(line)); } catch {} } }); }), 15000, "Studio startup");
    await refreshHost(); const originalHostPid = host.owner.pid;
    check(originalHostPid === server.pid, "Fixture owns the exact shared controller it will interrupt");
    const started = await action({op: "native_start", rendered: false}); evidence.native_pid = started.pid;
    for (const name of ["advance", "pause"]) { await action({op: "native_input", input_actions: [{action: name, pressed: true}]}); await action({op: "native_input", input_actions: [{action: name, pressed: false}]}); }
    const checkpoint = (await action({op: "native_checkpoint"})).checkpoint;
    check(started.running === true && checkpoint.paused === true && checkpoint.custom.counter === 8, "Real native game executes input and supplies a stable accepted checkpoint");
    const first = await submit(holdRequest("hold-a")); const firstIdentity = inspectProcess((await marker("hold-a")).pid); assert.ok(firstIdentity);
    const second = await submit(holdRequest("hold-b"));
    await until(async () => [await status(first), await status(second)], values => values.every(value => value.job?.state === "running"), "Two active manager slots");
    check(!(await exists(join(root, "hold-b.json"))), "Second active validation waits at the serial runtime Gate");
    const queued = []; for (let index = 0; index < 8; index++) queued.push(await submit({op: "write", path: `never-${index}.txt`, text: sourceSentinel}, 120));
    const saturated = await action(owned("job_list", null, {limit: 64}));
    check(saturated.jobs.filter(job => job.state === "running").length === 2 && saturated.jobs.filter(job => job.state === "queued").length === 8, "Shared service exposes two active and eight queued jobs");
    check((await http({op: "job_submit", owner_capability: owner, request: {op: "write", path: "overflow.txt", text: sourceSentinel}})).status === 400, "Saturated queue refuses additional work");
    const pollStart = Date.now(); await status(first); await action(owned("job_list", null, {limit: 2}));
    check(Date.now() - pollStart < 2000, "Compact status and list remain responsive while owned work blocks");
    const firstClient = mcp(); await firstClient.rpc("initialize", {protocolVersion: "2025-06-18", clientInfo: {name: "jobs-acceptance", version: "1"}, capabilities: {}});
    check((await firstClient.call(owned("job_status", second))).structuredContent.job.id === second.job_id, "MCP queries the HTTP-submitted job from the same manager");
    check((await firstClient.call(owned("job_cancel", second))).isError === true && (await cli(owned("job_cancel", second), true)).code !== 0, "Read-only MCP and CLI refuse cancellation before dispatch");
    check((await firstClient.call({op: "job_submit", request: {op: "read", path: "small.txt"}})).isError === true && (await cli({op: "job_submit", request: {op: "read", path: "small.txt"}}, true)).code !== 0, "Read-only interfaces refuse job admission");
    const foreign = hash(owner + "foreign");
    for (const op of ["job_status", "job_result", "job_cancel", "job_forget"]) check((await http({op, owner_capability: foreign, job_id: first.job_id})).status === 400, "Foreign capability cannot inspect or mutate another owner's job");
    check((await action({op: "job_list", owner_capability: foreign})).jobs.length === 0, "Foreign owner receives no job catalog entries");
    check((await http(owned("job_forget", first))).status === 400, "An active job cannot be forgotten");
    firstClient.child.stdin.end(); await bound(firstClient.done, 10000, "MCP disconnect");
    const secondClient = mcp(); await secondClient.rpc("initialize", {protocolVersion: "2025-06-18", clientInfo: {name: "jobs-reconnect", version: "1"}, capabilities: {}});
    check((await secondClient.call(owned("job_status", second))).structuredContent.job.id === second.job_id && (await secondClient.call({op: "native_status"})).structuredContent.pid === started.pid, "MCP reconnect retains the same job and accepted native PID");
    await refreshHost(); check(host.owner.pid === originalHostPid, "Client disconnect does not replace the shared controller");
    await summariesSafe();
    await action(owned("job_cancel", queued.shift()));
    const expiry = await submit({op: "write", path: "expired.txt", text: sourceSentinel}, 1), expired = await terminal(expiry);
    check(expired.job.state === "timed_out" && expired.job.started_ms === null && expired.job.committed === false && !(await exists(join(project, "expired.txt"))), "Queued deadline expires without starting or publishing the mutation");
    const unchanged = await action(owned("job_status", expiry, {after_sequence: expired.job.sequence}));
    check(unchanged.unchanged === true && !Object.hasOwn(unchanged, "job") && !Object.hasOwn(unchanged, "result") && Buffer.byteLength(JSON.stringify(unchanged)) < 1024, "Stable progress cursors return a compact unchanged reply");
    check((await http(owned("job_status", expiry, {after_sequence: expired.job.sequence + 1}))).status === 400 && (await http({op: "job_status", owner_capability: "a".repeat(63), job_id: expiry.job_id})).status === 400 && (await http(owned("job_status", "a".repeat(31)))).status === 400, "Future cursors and malformed capability/job identities are refused");
    for (const limit of [0, 65]) check((await http(owned("job_list", null, {limit}))).status === 400, "Catalog limits outside the declared range are refused");
    for (const job of queued) { await action(owned("job_cancel", job)); assert.equal((await terminal(job)).job.state, "cancelled"); }
    check((await secondClient.call(owned("job_cancel", second), false)).structuredContent.requested === true && (await terminal(second)).job.state === "cancelled" && !(await exists(join(root, "hold-b.json"))), "Cancelling a Gate waiter reaches a terminal state without spawning its worker");
    await action(owned("job_cancel", first)); const cancelled = await terminal(first); evidence.cancelled_validation = cancelled.job;
    check(cancelled.job.state === "cancelled" && cancelled.job.committed === false && cancelled.job.commit_status_verified === true && originalGone(firstIdentity), "Active validation cancellation confirms its witnessed owned worker terminated");
    check(!originalGone(witnessIdentity), "Validation cancellation leaves the witnessed unrelated process alive");
    for (const name of ["hold-a.gd", "hold-b.gd"]) assert.equal(await exists(join(project, "jobs", name)), false);
    for (let index = 0; index < 8; index++) assert.equal(await exists(join(project, `never-${index}.txt`)), false);
    check(!(await exists(join(project, "overflow.txt"))), "Queue refusal and cancellation publish none of the requested files");
    await preserved();

    for (const seconds of [0, 601]) check((await http({op: "job_submit", owner_capability: owner, timeout_seconds: seconds, request: {op: "status"}})).status === 400, "Job deadlines outside the declared bound are refused");
    const countBefore = (await action(owned("job_list", null, {limit: 64}))).jobs.length;
    const oversized = await http({op: "job_submit", owner_capability: owner, request: {op: "write", path: "oversized.txt", text: "X".repeat(2 * 1024 * 1024 + 1)}});
    check([400, 413].includes(oversized.status) && (await action(owned("job_list", null, {limit: 64}))).jobs.length === countBefore && !(await exists(join(project, "oversized.txt"))), "Oversized request is refused before admission under manager or transport bounds");
    check((await http({op: "job_submit", owner_capability: owner, request: {op: "read", project: root, path: "small.txt"}})).status === 400 && (await http({op: "job_submit", owner_capability: owner, request: {op: "job_list", owner_capability: owner}})).status === 400, "Nested jobs and project-scope changes are refused");
    const tinyTicket = await secondClient.call({op: "job_submit", owner_capability: owner, request: {op: "read", path: "small.txt"}}, false); assert.notEqual(tinyTicket.isError, true); assert.ok(/^[a-f0-9]{32}$/.test(tinyTicket.structuredContent.job_id));
    const tiny = {job_id: tinyTicket.structuredContent.job_id}; submitted.push(tiny.job_id); await terminal(tiny);
    const cliResult = await cli(owned("job_result", tiny), true), tinyResult = cliResult.value;
    const expectedSmall = JSON.stringify({path: "small.txt", sha256: hash(smallText), text: smallText});
    check(cliResult.code === 0 && tinyResult.job.state === "succeeded" && tinyResult.job.result_available === true && tinyResult.result.text === smallText && tinyResult.job.result_bytes === Buffer.byteLength(expectedSmall) && tinyResult.job.result_sha256 === hash(expectedSmall) && tinyResult.job.result_complete === true, "CLI retrieves the MCP-submitted retained result and independently verified serialized hash");
    check(!Object.hasOwn(await status(tiny), "result"), "Status polling omits retained source text even after completion");
    const large = await submit({op: "read", path: "large.txt"}); await terminal(large); const largeResult = await action(owned("job_result", large));
    const expectedLarge = JSON.stringify({path: "large.txt", sha256: hash(files["large.txt"]), text: files["large.txt"]});
    check(largeResult.job.state === "succeeded" && largeResult.job.result_available === false && largeResult.result === null && largeResult.job.result_truncated === true && largeResult.job.result_complete === true && largeResult.job.result_bytes_exact === true && largeResult.job.result_sha256 === hash(expectedLarge), "Over-retention result keeps a complete independent hash without retaining its source body");
    const huge = await submit({op: "read", path: "huge.txt"}); await terminal(huge); const hugeResult = await action(owned("job_result", huge));
    check(hugeResult.job.state === "succeeded" && hugeResult.job.result_available === false && hugeResult.job.result_truncated === true && hugeResult.job.result_complete === false && hugeResult.job.result_bytes_exact === false && hugeResult.job.result_sha256 === null && hugeResult.job.result_bytes <= 2 * 1024 * 1024, "Result beyond the streaming budget is explicitly incomplete and has no full-result hash");
    check((await action(owned("job_forget", huge))).forgotten === true && (await http(owned("job_status", huge))).status === 400, "Explicit forget removes only a terminal job receipt");

    const bootMarker = join(root, "native-cancel.json").replaceAll("\\", "/");
    const blockedMain = main + `\nfunc _init():\n\tvar file = FileAccess.open(${JSON.stringify(bootMarker)}, FileAccess.WRITE)\n\tif file == null: return\n\tfile.store_string(JSON.stringify({"pid": OS.get_process_id()}))\n\tfile.close()\n\tOS.delay_msec(120000)\n`;
    await writeFile(join(project, "main.gd"), blockedMain);
    const replacement = await submit({op: "native_replace"}); const replacementIdentity = inspectProcess((await marker("native-cancel")).pid); assert.ok(replacementIdentity);
    await action(owned("job_cancel", replacement)); const replacementResult = await terminal(replacement); evidence.cancelled_replacement = replacementResult.job;
    check(replacementResult.job.state === "cancelled" && replacementResult.job.committed === false && replacementResult.job.commit_status_verified === true && originalGone(replacementIdentity), "Native replacement cancellation confirms the prepared owned process terminated before commit");
    check((await action({op: "native_status"})).pid === started.pid, "Cancelled replacement retains the accepted native process");
    assert.deepEqual((await action({op: "native_checkpoint"})).checkpoint, checkpoint); checks++;
    await preserved(blockedMain); check(!originalGone(witnessIdentity), "Native cancellation preserves working source/DLL and the unrelated process");
    await writeFile(join(project, "main.gd"), main);

    let latest;
    for (let index = 0; index < 65; index++) { latest = await submit({op: "read", path: "small.txt"}); assert.equal((await terminal(latest)).job.state, "succeeded"); }
    const bounded = await action(owned("job_list", null, {limit: 64}));
    check(bounded.jobs.length === 64 && bounded.jobs.every(job => job.terminal === true) && (await http(owned("job_status", first))).status === 400, "Terminal history retains exactly the bounded catalog and prunes old receipt identities");
    const limited = await action(owned("job_list", null, {limit: 1})); check(limited.jobs.length === 1 && limited.truncated === true, "Catalog pagination reports explicit truncation");
    await summariesSafe();
    const crashFirst = await submit(holdRequest("crash-a")), crashIdentity = inspectProcess((await marker("crash-a")).pid); assert.ok(crashIdentity);
    const crashSecond = await submit(holdRequest("crash-b")); await until(() => status(crashSecond), value => value.job?.state === "running", "Crash Gate waiter");
    const crashQueued = await submit({op: "write", path: "never-replayed.txt", text: sourceSentinel}); check((await status(crashQueued)).job.state === "queued", "Crash mutation is durably queued before host interruption");
    server.kill("SIGKILL"); await bound(serverDone, 10000, "Controller interruption");
    check(originalGone(crashIdentity) && !originalGone(witnessIdentity), "Controller crash terminates its owned worker and preserves the unrelated witness");
    check((await cli(owned("job_status", crashFirst), true)).value.host_available === false, "Read-only inspection does not silently recreate the dead controller");
    const reconnected = await cli(owned("job_reconnect", null, {limit: 64})); assert.equal(reconnected.code, 0);
    await refreshHost(); check(host.owner.pid !== originalHostPid, "Explicit reconnect creates a new authenticated shared controller");
    for (const job of [crashFirst, crashSecond, crashQueued]) { const result = await action(owned("job_result", job)); check(result.job.state === "interrupted" && result.job.terminal === true && result.job.commit_status_verified === false && result.summary_only === true && result.job.result_available === false, "Reconnect labels interrupted work with unverified commit state and never reconstructs a request"); }
    const recovered = await action(owned("job_result", latest));
    check(recovered.job.state === "succeeded" && recovered.job.result_available === false && recovered.summary_only === true && recovered.job.result_sha256 === hash(expectedSmall), "Reconnect preserves completed result hashes while retained result bodies remain unavailable");
    check(!(await exists(join(root, "crash-b.json"))) && !(await exists(join(project, "never-replayed.txt"))) && (await action({op: "native_status"})).running !== true, "Reconnect replays neither the Gate waiter, queued mutation nor interrupted native game");
    check((await secondClient.call(owned("job_status", crashQueued))).structuredContent.job.state === "interrupted", "Existing MCP client routes to recovered shared summaries after controller replacement");
    await summariesSafe(); await preserved();
    // Preset configuration is a deliberate source mutation; compare export's
    // private import work against the configured source/cache state afterward.
    const preset = await action({op: "configure_export", platform: "Windows Desktop"});
    check(preset.ok === true && typeof preset.preset === "string", "Export preset is configured synchronously before job admission");
    const captureExportSource = async () => {
        const entries = []; let bytes = 0;
        const walk = async (directory, prefix = "", depth = 0) => {
            assert.ok(depth <= 32, "Export source inventory remains within its depth bound");
            for (const name of (await readdir(directory)).sort()) {
                if (!prefix && [".aurum", "dist"].includes(name)) continue;
                const path = join(directory, name), relative = prefix + name, metadata = await lstat(path);
                assert.equal(metadata.isSymbolicLink(), false, "Export source inventory must not follow links");
                assert.ok(entries.length < 4096, "Export source inventory remains within its entry bound");
                if (metadata.isDirectory()) { entries.push([relative + "/", "directory"]); await walk(path, relative + "/", depth + 1); }
                else { assert.equal(metadata.isFile(), true, "Export source inventory contains only regular files"); bytes += metadata.size; assert.ok(bytes <= 64 * 1024 * 1024, "Export source inventory remains within its byte bound"); entries.push([relative, hash(await readFile(path))]); }
            }
        };
        await walk(project); return entries;
    };
    const configuredSource = await captureExportSource();
    const exported = await submit({op: "export", preset: preset.preset, output: "dist/job-export/game.pck", pack: true});
    await terminal(exported); const exportResult = await action(owned("job_result", exported));
    const packPath = join(project, "dist/job-export/game.pck"), pack = await readFile(packPath);
    check(exportResult.job.state === "succeeded" && exportResult.job.committed === true && exportResult.job.commit_status_verified === true && exportResult.result?.ok === true && exportResult.result.atomic_directory_publication === true && pack.length > 100, "Actual export-pack job publishes a complete fresh directory and confirms its commit");
    assert.deepEqual(await captureExportSource(), configuredSource, "Successful async export preserves working source, UID files and import-cache contents"); checks++;
    evidence.export_job = exportResult.job; evidence.export_sha256 = hash(pack);
    const refusedExport = await submit({op: "export", preset: preset.preset, output: "dist/job-export/refused.pck", pack: true});
    await terminal(refusedExport); const refusedResult = await action(owned("job_result", refusedExport));
    check(refusedResult.job.state === "failed" && refusedResult.job.committed === false && refusedResult.job.commit_status_verified === true && refusedResult.result?.ok === false && !(await exists(join(project, "dist/job-export/refused.pck"))) && hash(await readFile(packPath)) === hash(pack), "Existing export parent is refused before commit and preserves the published pack");
    assert.deepEqual(await captureExportSource(), configuredSource, "Refused async export preserves working source, UID files and import-cache contents"); checks++;
    for (const [path, text] of Object.entries(files)) if (!path.startsWith(".godot/")) assert.equal(hash(await readFile(join(project, path))), hash(text), `Export preserves working source/DLL: ${path}`);
    checks++;
    const packageTicket = await submit({op: "package", output: "dist/job-package"});
    await until(() => status(packageTicket), reply => reply.job?.terminal === true, "Terminal package job receipt", 60000);
    const packageResult = await action(owned("job_result", packageTicket)), packaged = packageResult.result;
    await writeFile(join(root, "package-job-result.json"), JSON.stringify(packageResult, null, 2));
    check(packageResult.job.state === "succeeded" && packageResult.job.committed === true && packageResult.job.commit_status_verified === true && packaged?.ok === true && packaged.requires_separate_godot === false && packaged.private_source_cleaned === true, "Async package publishes a verified committed bundle and cleans its private source");
    const packageDirectory = join(project, "dist/job-package"), packagePack = join(packageDirectory, "game.pck"), packageExe = join(packageDirectory, "game.exe");
    check(await realpath(packaged.directory) === await realpath(packageDirectory) && await realpath(packaged.executable) === await realpath(packageExe), "Package receipt resolves to the requested physical directory and its expected game.exe");
    const packagePackMetadata = await lstat(packagePack), packageExeMetadata = await lstat(packageExe);
    check(packagePackMetadata.isFile() && !packagePackMetadata.isSymbolicLink() && packagePackMetadata.size > 0 && packageExeMetadata.isFile() && !packageExeMetadata.isSymbolicLink() && packageExeMetadata.size > 0, "Published package contains nonempty regular PCK and runtime executable files");
    const packageLaunch = spawnSync(packageExe, ["--headless", "--path", packageDirectory, "--main-pack", packagePack, "--quit-after", "2"], {env: environment, windowsHide: true, encoding: "utf8", timeout: 30000, maxBuffer: 4 * 1024 * 1024});
    await writeFile(join(root, "package-launch.log"), (packageLaunch.stdout ?? "") + (packageLaunch.stderr ?? ""));
    assert.ifError(packageLaunch.error);
    check(packageLaunch.status === 0 && !/\b(?:SCRIPT ERROR|ERROR):/.test((packageLaunch.stdout ?? "") + (packageLaunch.stderr ?? "")), "Bundled executable loads the published pack headlessly within its budget without runtime errors");
    assert.deepEqual(await captureExportSource(), configuredSource, "Async package and standalone execution preserve working source, UID files and import-cache contents"); checks++;
    evidence.package_job = packageResult.job; evidence.package_sha256 = hash(await readFile(packagePack)); evidence.package_runtime_sha256 = packaged.runtime_sha256; evidence.package_launch_exit = packageLaunch.status;
    await summariesSafe(); evidence.ok = true;
} catch (error) { evidence.ok = false; evidence.error = error.stack ?? String(error); throw error; }
finally {
    for (const client of clients) if (client.child.exitCode === null && client.child.signalCode === null) { client.child.stdin.end(); try { await bound(client.done, 10000, "MCP closure"); } catch { client.child.kill(); await bound(client.done, 5000, "MCP termination"); } }
    try { await refreshHost(); await fetch(`http://127.0.0.1:${host.port}/api/stop`, {method: "POST", headers: {"X-Aurum-Token": token}, signal: AbortSignal.timeout(5000)}); } catch {}
    if (server && server.exitCode === null && server.signalCode === null) { try { await bound(serverDone, 10000, "Studio closure"); } catch { server.kill(); await bound(serverDone, 5000, "Studio termination"); } }
    if (witness && witness.exitCode === null && witness.signalCode === null) witness.kill();
    evidence.checks = checks; evidence.job_ids = submitted;
    await writeFile(join(root, "studio-stderr.log"), stderr); await writeFile(join(root, "verification.json"), JSON.stringify(evidence, null, 2));
}
console.log(JSON.stringify({checks, status: "passed", evidence: root}));
