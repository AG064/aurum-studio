// Metadata-only target reports through actual read-only CLI, HTTP and MCP.
import assert from "node:assert/strict";
import {spawn} from "node:child_process";
import {mkdtemp, mkdir, writeFile, readFile, readdir, lstat} from "node:fs/promises";
import {createHash} from "node:crypto";
import {tmpdir} from "node:os";
import {join, resolve, delimiter} from "node:path";

const [binary, godot] = process.argv.slice(2);
assert.ok(binary && godot, "Usage: target-preflight-acceptance.mjs <aurum> <godot>");
assert.equal(process.platform, "win32", "This fixture qualifies Windows runtime version-resource metadata");
const root = await mkdtemp(join(tmpdir(), "aurum-target-preflight-")), project = join(root, "project");
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const keySentinel = "PRIVATE-KEY-CONTENT-PREFLIGHT-SENTINEL", passwordSentinel = "PRIVATE-PRESET-PASSWORD-PREFLIGHT-SENTINEL";
const credentialSentinel = "PRIVATE-REQUEST-CREDENTIAL-PREFLIGHT-SENTINEL";
const paths = {sdk: join(root, "sdks/complete"), mismatchSdk: join(root, "sdks/mismatch"), jdk: join(root, "jdks/17"), mismatchJdk: join(root, "jdks/11"), unknownJdk: join(root, "jdks/unknown"), templates: join(root, "templates"), key: join(root, "keys/debug.keystore")};
const environment = {...process.env, AURUM_GODOT: resolve(godot), AURUM_STUDIO_HOME: join(root, "state"), APPDATA: join(root, "user"), LOCALAPPDATA: join(root, "user"), USERPROFILE: join(root, "user"), ANDROID_SDK_ROOT: paths.sdk, ANDROID_HOME: paths.sdk, JAVA_HOME: paths.jdk, GODOT_ANDROID_KEYSTORE_DEBUG_PATH: paths.key, GODOT_ANDROID_KEYSTORE_RELEASE_PATH: "", TEMP: join(root, "tmp"), TMP: join(root, "tmp"), PATH: [join(paths.sdk, "platform-tools"), join(paths.jdk, "bin"), process.env.PATH ?? ""].join(delimiter)};
const bound = (promise, ms, label) => new Promise((resolve, reject) => { const timer = setTimeout(() => reject(new Error(`${label} deadline exceeded`)), ms); promise.then(value => { clearTimeout(timer); resolve(value); }, error => { clearTimeout(timer); reject(error); }); });
const save = async (path, text) => { await mkdir(join(path, ".."), {recursive: true}); await writeFile(path, text); };
async function protectedFileDigest(path, metadata) {
    assert.ok(metadata.isFile() && !metadata.isSymbolicLink(), "Protected files must be regular and not linked");
    if (metadata.size !== 0) return hash(await readFile(path));
    // Windows byte-range leases can refuse even an empty read. Preserve the
    // entry and prove its empty bytes and exact metadata without opening it.
    const before = await lstat(path, {bigint: true});
    assert.ok(before.isFile() && !before.isSymbolicLink() && before.size === 0n, "Empty protected file must remain regular and empty");
    const sha256 = hash(Buffer.alloc(0)), after = await lstat(path, {bigint: true});
    assert.ok(after.isFile() && !after.isSymbolicLink(), "Empty protected file must remain regular and not linked");
    const identity = {};
    for (const key of ["dev", "ino", "size", "birthtimeNs", "ctimeNs", "mtimeNs", "atimeNs"]) {
        assert.equal(after[key], before[key], `Empty protected file ${key} must not drift`);
        identity[key] = before[key].toString();
    }
    return {sha256, identity};
}
const inventory = async directory => {
    const rows = []; let bytes = 0;
    const walk = async (directory, prefix, depth) => {
        assert.ok(depth <= 32, "Protected fixture depth remains bounded");
        for (const name of (await readdir(directory)).sort()) {
            const path = join(directory, name), relative = prefix + name, metadata = await lstat(path);
            assert.ok(rows.length < 4096 && !metadata.isSymbolicLink(), "Protected fixtures remain bounded and contain no links");
            if (metadata.isDirectory()) { rows.push([relative + "/", "directory"]); await walk(path, relative + "/", depth + 1); }
            else { assert.ok(metadata.isFile()); bytes += metadata.size; assert.ok(bytes <= 16 * 1024 * 1024); rows.push([relative, await protectedFileDigest(path, metadata)]); }
        }
    };
    await walk(directory, "", 0); return rows;
};
const runCli = input => new Promise((resolveReply, reject) => {
    const child = spawn(resolve(binary), ["project", project, "--read-only", "--request-json", JSON.stringify(input)], {env: environment, windowsHide: true});
    let stdout = "", stderr = "";
    child.stdout.on("data", data => { stdout += data; if (Buffer.byteLength(stdout) > 256 * 1024) { child.kill(); reject(new Error("CLI report exceeded its byte bound")); } });
    child.stderr.on("data", data => { stderr = (stderr + data).slice(-65536); }); child.once("error", reject);
    const timer = setTimeout(() => { child.kill(); reject(new Error("CLI preflight deadline exceeded")); }, 20000);
    child.once("close", code => { clearTimeout(timer); let value; try { value = JSON.parse(stdout); } catch { value = {error: stderr}; } resolveReply({code, value}); });
});
let checks = 0, server, serverDone, address, serverStderr = "", mcpChild, mcpDone;
const evidence = {scope: "read-only metadata availability through compiled interfaces; no SDK installation, export, signing or device qualification"};
const check = (condition, message) => { assert.ok(condition, message); checks++; };
const row = (report, id) => { const result = report.checks.find(check => check.id === id); assert.ok(result, `Report includes ${id}`); return result; };
const readOnlyReport = report => {
    check(report.ok === true && report.read_only === true && report.report_version === 1 && report.tools_invoked === false && report.network_used === false && report.private_keys_read === false && report.installed === false && report.workspace_changed === false && report.device_changed === false && report.device_query_performed === false && report.devices === null, "Metadata report explicitly performs no tool, network, key or device actions");
    check(report.export_readiness === "unverified" && report.physical_readiness === "unverified" && report.signing_verified === false && report.deployed === false, "Detected prerequisite files never assert export, signing, deployment or physical readiness");
    const text = JSON.stringify(report); check(Buffer.byteLength(text) <= 128 * 1024 && ![keySentinel, passwordSentinel, credentialSentinel].some(value => text.includes(value)), "Bounded reports omit keystore contents and supplied credentials");
};
const good = {op: "target_preflight", platform: "Android", preset: "Android", godot_path: resolve(godot), templates_path: paths.templates, android_sdk_path: paths.sdk, java_sdk_path: paths.jdk, keystore_path: paths.key};
const cases = [];
try {
    for (const directory of [project, join(root, "sdks"), join(root, "jdks"), paths.templates, join(root, "keys"), join(root, "state"), join(root, "user"), join(root, "tmp")]) await mkdir(directory, {recursive: true});
    await save(join(project, "aurum.toml"), 'schema_version=1\nname="preflight-fixture"\ngodot_version="4.7"\nmodules=[]\n');
    await save(join(project, "project.godot"), 'config_version=5\n[application]\nconfig/name="Preflight fixture"\n');
    const q = value => JSON.stringify(value.replaceAll("\\", "/"));
    await save(join(project, "export_presets.cfg"), `[preset.0]\nname="Android"\nplatform="Android"\n[preset.0.options]\ngradle_build/use_gradle_build=false\nkeystore/debug=${q(paths.key)}\nkeystore/debug_password="${passwordSentinel}"\nkeystore/release_password="${passwordSentinel}"\n[preset.1]\nname="Android Custom"\nplatform="Android"\n[preset.1.options]\ncustom_template/debug="res://custom.apk"\n[preset.2]\nname="Windows"\nplatform="Windows Desktop"\n[preset.3]\nname="iOS"\nplatform="iOS"\n`);
    await save(join(project, "custom.apk"), "synthetic custom template\n"); await save(join(project, "Godot_v4.7-fake.exe"), "filename is not version evidence\n");
    await save(paths.key, keySentinel); await save(join(paths.templates, "4.7.stable/android_debug.apk"), "synthetic template\n");
    await save(join(paths.templates, "4.6.stable/android_debug.apk"), "synthetic older template\n");
    const sdk = async (directory, revision, api) => {
        await save(join(directory, "platform-tools/adb.exe"), "synthetic tool, never execute\n");
        await save(join(directory, "platform-tools/source.properties"), `Pkg.Revision=${revision === "35.0.1" ? "37.0.1" : "34.0.5"}\n`);
        await save(join(directory, `build-tools/${revision}/source.properties`), `Pkg.Revision=${revision}\n`);
        for (const name of ["apksigner.bat", "zipalign.exe", "aapt2.exe"]) await save(join(directory, `build-tools/${revision}/${name}`), "synthetic tool, never execute\n");
        await save(join(directory, `platforms/android-${api}/source.properties`), `AndroidVersion.ApiLevel=${api}\nPkg.Revision=2\n`);
        await save(join(directory, `platforms/android-${api}/android.jar`), "synthetic platform library, metadata presence only\n");
        await save(join(directory, "cmdline-tools/latest/bin/sdkmanager.bat"), "synthetic tool, never execute\n");
        await save(join(directory, "cmdline-tools/latest/source.properties"), "Pkg.Revision=19.0\n");
    };
    await sdk(paths.sdk, "35.0.1", "35"); await sdk(paths.mismatchSdk, "35.0.0", "34");
    for (const [directory, release] of [[paths.jdk, 'JAVA_VERSION="17.0.20.1"\n'], [paths.mismatchJdk, 'JAVA_VERSION="11.0.28"\n'], [paths.unknownJdk, "X".repeat(65537)]]) {
        await save(join(directory, "release"), release); for (const name of ["java.exe", "keytool.exe", "jarsigner.exe"]) await save(join(directory, "bin", name), "synthetic tool, never execute\n");
    }
    await save(join(root, "state/runtime/templates/4.7.stable/android_debug.apk"), "managed fallback sentinel\n");
    const protectedRoots = [project, join(root, "sdks"), join(root, "jdks"), paths.templates, join(root, "keys"), join(root, "state"), join(root, "user"), join(root, "tmp")];
    const capture = async () => Promise.all(protectedRoots.map(inventory));
    const initial = await capture();
    cases.push({name: "complete", request: good, verify: report => check(report.prerequisites_detected === true && row(report, "runtime_version").status === "available" && /^4\.7\./.test(report.runtime_version) && row(report, "android_build_tools").status === "available" && row(report, "java_version").status === "available" && row(report, "signing_keystore_presence").status === "available", "Real Godot metadata plus complete synthetic prerequisites are detected without execution")});
    cases.push({name: "explicit-missing-sdk", request: {...good, android_sdk_path: join(root, "sdks/missing")}, verify: report => check(row(report, "android_sdk").status === "missing" && row(report, "android_sdk").provenance === "configured" && row(report, "android_sdk").path.replaceAll("\\", "/").toLowerCase().endsWith("/sdks/missing") && report.prerequisites_detected === false, "Missing explicit SDK stays authoritative over valid environment defaults")});
    cases.push({name: "mismatch", request: {...good, android_sdk_path: paths.mismatchSdk, java_sdk_path: paths.mismatchJdk}, verify: report => check(row(report, "android_build_tools").status === "mismatch" && row(report, "android_platform").status === "mismatch" && row(report, "platform_tools_version").status === "mismatch" && row(report, "java_version").status === "mismatch", "Detected incompatible SDK and JDK versions are distinguished from missing files")});
    cases.push({name: "unknown-runtime", request: {...good, godot_path: join(project, "Godot_v4.7-fake.exe")}, verify: report => check(report.runtime_version === null && row(report, "runtime_version").status === "unverified" && row(report, "android_requirement_policy").status === "unverified" && row(report, "export_template").status === "unverified", "Unknown runtime cannot borrow requirements from project settings or filename spelling")});
    cases.push({name: "unknown-java", request: {...good, java_sdk_path: paths.unknownJdk}, verify: report => check(row(report, "java_version").status === "unverified" && report.prerequisites_detected === false, "Oversized JDK metadata is unverified rather than guessed")});
    cases.push({name: "missing-template", request: {...good, templates_path: join(root, "templates/missing")}, verify: report => check(row(report, "export_template").status === "missing" && row(report, "export_template").provenance === "configured", "Missing configured templates never silently use a managed fallback")});
    cases.push({name: "template-version", request: {...good, templates_path: join(paths.templates, "4.6.stable")}, verify: report => check(row(report, "template_version").status === "mismatch" && report.prerequisites_detected === false, "Template/runtime version disagreement is explicit")});
    cases.push({name: "gradle", request: {...good, gradle: true}, verify: report => check(row(report, "android_export_mode").status === "mismatch" && row(report, "android_gradle_template").status === "missing", "Gradle mode disagreement and missing source template are explicit")});
    cases.push({name: "custom-template", request: {...good, preset: "Android Custom"}, verify: report => check(row(report, "custom_export_template_presence").status === "available" && row(report, "export_template").status === "unverified", "Custom template presence does not claim version compatibility")});
    cases.push({name: "ios", request: {...good, platform: "iOS", preset: "iOS"}, verify: report => check(row(report, "ios_host_toolchain").status === "unverified" && report.physical_readiness === "unverified", "iOS host, signing and device readiness remain unverified")});
    const cliReports = new Map();
    for (const test of cases) { const reply = await runCli(test.request); assert.equal(reply.code, 0, JSON.stringify(reply.value)); readOnlyReport(reply.value); test.verify(reply.value); cliReports.set(test.name, reply.value); }
    const invalid = [{...good, platform: "unsupported"}, {...good, debug: "true"}, {...good, gradle: "false"}, {...good, deploy: true}, {...good, install: true}, {...good, device_id: "emulator-5554"}, {...good, keystore_password: credentialSentinel}, {...good, token: credentialSentinel}, {...good, android_sdk_path: "https://example.invalid/sdk"}, {...good, templates_path: "//server/templates"}, {...good, preset: "bad\nname"}];
    for (const request of invalid) { const reply = await runCli(request); check(reply.code !== 0 && !JSON.stringify(reply.value).includes(credentialSentinel), "Read-only CLI strictly refuses invalid flags, credentials and remote/control-character paths"); }
    assert.deepEqual(await capture(), initial, "CLI reports preserve entire protected fixture trees"); checks++;
    check(!(await readdir(project)).includes(".aurum") && !(await readdir(project)).includes(".godot") && (await readdir(join(root, "state"))).length === 1 && (await readdir(join(root, "tmp"))).length === 0, "Read-only CLI creates no controller, diagnostics, baseline, import cache or private workspace");
    evidence.complete_metadata = cliReports.get("complete"); evidence.unknown_runtime = cliReports.get("unknown-runtime");

    server = spawn(resolve(binary), ["studio", project, "--no-open", "--json", "--port", "0"], {env: environment, windowsHide: true});
    serverDone = new Promise(resolve => server.once("close", resolve)); server.stderr.on("data", data => { serverStderr = (serverStderr + data).slice(-65536); });
    address = await bound(new Promise((resolve, reject) => { let output = ""; server.once("error", reject); server.stdout.on("data", data => { output = (output + data).slice(-65536); for (const line of output.split("\n")) if (line.startsWith("{")) { try { resolve(JSON.parse(line)); } catch {} } }); }), 15000, "Transport host startup");
    const url = new URL(address.url), token = url.searchParams.get("t");
    const http = async input => { const response = await fetch(`${url.origin}/api/project`, {method: "POST", headers: {"Content-Type": "application/json", "X-Aurum-Token": token}, body: JSON.stringify(input), signal: AbortSignal.timeout(20000)}); return {status: response.status, value: await response.json()}; };
    mcpChild = spawn(resolve(binary), ["mcp", "--root", project, "--tools", "studio", "--read-only"], {env: environment, windowsHide: true});
    mcpDone = new Promise(resolve => mcpChild.once("close", resolve)); mcpChild.stderr.on("data", () => {});
    let buffer = "", rpcId = 0; const pending = new Map();
    const fail = error => { for (const entry of pending.values()) entry.reject(error); pending.clear(); mcpChild.kill(); };
    mcpChild.once("error", fail); mcpChild.stdout.on("data", data => { try { buffer += data; if (Buffer.byteLength(buffer) > 256 * 1024) throw new Error("MCP report framing exceeded bound"); for (;;) { const end = buffer.indexOf("\n"); if (end < 0) break; const line = buffer.slice(0, end); buffer = buffer.slice(end + 1); if (!line.trim()) continue; const reply = JSON.parse(line), entry = pending.get(reply.id); if (entry) { pending.delete(reply.id); entry.resolve(reply); } } } catch (error) { fail(error); } });
    const rpc = (method, params) => { const id = ++rpcId, result = new Promise((resolve, reject) => pending.set(id, {resolve, reject})); mcpChild.stdin.write(`${JSON.stringify({jsonrpc: "2.0", id, method, params})}\n`); return bound(result, 20000, "Read-only MCP response"); };
    await rpc("initialize", {protocolVersion: "2025-06-18", clientInfo: {name: "target-preflight-acceptance", version: "1"}, capabilities: {}});
    const transportBaseline = await capture();
    for (const test of cases) {
        const report = await http(test.request); assert.equal(report.status, 200, JSON.stringify(report.value)); assert.deepEqual(report.value, cliReports.get(test.name)); checks++;
        const tool = (await rpc("tools/call", {name: "aurum_project_query", arguments: test.request})).result;
        assert.notEqual(tool.isError, true); assert.deepEqual(tool.structuredContent, cliReports.get(test.name)); checks++;
    }
    for (const request of invalid) {
        const reply = await http(request); check(reply.status === 400 && !JSON.stringify(reply.value).includes(credentialSentinel), "HTTP refuses the same strict read-only request errors");
        const tool = (await rpc("tools/call", {name: "aurum_project_query", arguments: request})).result;
        check(tool.isError === true && !JSON.stringify(tool).includes(credentialSentinel), "Read-only MCP refuses malformed/credential input without echoing credentials");
    }
    assert.deepEqual(await capture(), transportBaseline, "HTTP/MCP reports create no additional private state and preserve every protected fixture tree"); checks++;
    evidence.ok = true;
} catch (error) { evidence.ok = false; evidence.error = error.stack ?? String(error); throw error; }
finally {
    if (mcpChild && mcpChild.exitCode === null && mcpChild.signalCode === null) { mcpChild.stdin.end(); try { await bound(mcpDone, 10000, "MCP closure"); } catch { mcpChild.kill(); await bound(mcpDone, 5000, "MCP termination"); } }
    if (address) { const url = new URL(address.url); try { await fetch(`${url.origin}/api/stop`, {method: "POST", headers: {"X-Aurum-Token": url.searchParams.get("t")}, signal: AbortSignal.timeout(5000)}); } catch {} }
    if (server && server.exitCode === null && server.signalCode === null) { try { await bound(serverDone, 10000, "Host closure"); } catch { server.kill(); await bound(serverDone, 5000, "Host termination"); } }
    evidence.checks = checks; await writeFile(join(root, "studio-stderr.log"), serverStderr); await writeFile(join(root, "verification.json"), JSON.stringify(evidence, null, 2));
}
console.log(JSON.stringify({checks, status: "passed", evidence: root}));
