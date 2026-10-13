// Actual nested Cargo workspace preparation and guarded native source revisions.
import assert from "node:assert/strict";
import {spawn, spawnSync} from "node:child_process";
import {mkdtemp, mkdir, writeFile, readFile, readdir, lstat, symlink, unlink} from "node:fs/promises";
import {createHash} from "node:crypto";
import {tmpdir} from "node:os";
import {join, resolve, dirname} from "node:path";
import {fileURLToPath} from "node:url";

const [binary, godot] = process.argv.slice(2);
assert.ok(binary && godot, "Usage: native-workspace-acceptance.mjs <aurum> <godot>");
assert.equal(process.platform, "win32", "This fixture qualifies the Windows native workspace path");
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const locked = await readFile(join(repo, "Cargo.lock"), "utf8");
const pinned = locked.match(/name = "godot"[\s\S]*?source = "git\+https:\/\/github.com\/godot-rust\/gdext[^#]*#([a-f0-9]{40})"/);
assert.ok(pinned, "Repository lockfile contains a pinned gdext revision");
const root = await mkdtemp(join(tmpdir(), "aurum-native-workspace-"));
const project = join(root, "project"), workspace = join(project, "rust"), godotRoot = join(project, "godot");
const sourcePath = "rust/game/src/lib.rs", libraryPath = "godot/addons/native/bin/active.dll";
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const config = hint => `schema_version=1\nname="native-workspace-fixture"\nrust_package="aurum-workspace-game"\naddon_destination="godot/addons/native"\nmodules=[]\n[engine]\npath_hint=${JSON.stringify(hint.replaceAll("\\", "/"))}\n`;
const originalConfig = config("rust");
const rust = `use godot::prelude::*;
use godot::classes::Node;
#[derive(GodotClass)]
#[class(base=Node)]
struct WorkspaceState {
    #[export] counter: i64,
    #[export] build_revision: i64,
    base: Base<Node>,
}
#[godot_api]
impl INode for WorkspaceState {
    fn init(base: Base<Node>) -> Self { Self { counter: 10, build_revision: 1, base } }
}
#[godot_api]
impl WorkspaceState {
    #[func] fn advance(&mut self) { self.counter += self.build_revision * nested_support::step(); }
}
struct NestedGame;
#[gdextension]
unsafe impl ExtensionLibrary for NestedGame {}
`;
const changed = rust.replace("build_revision: 1", "build_revision: 2");
const support = "pub fn step() -> i64 { 3 }\n";
const changedSupport = "pub fn step() -> i64 { 5 }\n";
const script = `extends Node3D
var inventory = ["battery", "cargo"]
func _ready():
\tprocess_mode = Node.PROCESS_MODE_ALWAYS
func _input(event):
\tif event.is_action_pressed("pause"): get_tree().paused = not get_tree().paused
\tif event.is_action_pressed("advance") and not get_tree().paused: $WorkspaceState.advance()
func aurum_capture_state():
\treturn {"schema": 1, "position": position, "inventory": inventory, "counter": $WorkspaceState.counter}
func aurum_restore_state(state):
\tif not state is Dictionary or state.get("schema") != 1 or not state.get("position") is Vector3 or not state.get("inventory") is Array: return false
\tvar counter = state.get("counter")
\tif not typeof(counter) in [TYPE_INT, TYPE_FLOAT] or not is_finite(float(counter)) or counter != floor(counter): return false
\tposition = state.position
\tinventory = state.inventory.duplicate()
\t$WorkspaceState.counter = int(counter)
\treturn true
`;
const environment = {...process.env, AURUM_GODOT: resolve(godot), AURUM_STUDIO_HOME: join(root, "state"),
    APPDATA: join(root, "user"), LOCALAPPDATA: join(root, "user"), CARGO_TARGET_DIR: join(root, "fixture-target")};
const bound = (promise, ms, label) => new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`${label} deadline exceeded`)), ms);
    promise.then(value => { clearTimeout(timer); resolve(value); }, error => { clearTimeout(timer); reject(error); });
});
const pinnedPackages = text => new Map(text.split("[[package]]").flatMap(block => {
    const field = name => block.match(new RegExp(`^${name} = "([^"]+)"`, "m"))?.[1];
    const source = field("source");
    return source ? [[`${field("name")}@${source}`, `${field("version")}#${field("checksum") ?? ""}`]] : [];
}));
const cacheInventory = async directory => {
    const entries = [];
    const walk = async (directory, prefix) => {
        for (const name of (await readdir(directory)).sort()) {
            const path = join(directory, name), relative = prefix + name, metadata = await lstat(path);
            assert.equal(metadata.isSymbolicLink(), false, "Working cache inventory must not follow links");
            if (metadata.isDirectory()) { entries.push([relative + "/", "directory"]); await walk(path, relative + "/"); }
            else { assert.equal(metadata.isFile(), true, "Working caches contain only regular files"); entries.push([relative, hash(await readFile(path))]); }
        }
    };
    await walk(directory, "");
    return entries;
};
let checks = 0, server, done, address, stderr = "";
const evidence = {scope: "contained nested Cargo workspace and native source revisions; external workspace staging and device acceptance remain open"};
const check = (condition, message) => { assert.ok(condition, message); checks++; };

try {
    await mkdir(join(workspace, "game/src"), {recursive: true});
    await mkdir(join(workspace, "support/src"), {recursive: true});
    await mkdir(join(godotRoot, "addons/native/bin"), {recursive: true});
    await mkdir(join(godotRoot, ".godot"), {recursive: true});
    await mkdir(join(workspace, "target"), {recursive: true});
    await mkdir(join(root, "outside"), {recursive: true});
    await writeFile(join(project, "aurum.toml"), originalConfig);
    await writeFile(join(workspace, "Cargo.toml"), `[workspace]\nresolver="2"\nmembers=["game","support"]\n[profile.dev]\ndebug=0\nincremental=false\ncodegen-units=1\n`);
    await writeFile(join(workspace, "game/Cargo.toml"), `[package]\nname="aurum-workspace-game"\nversion="0.1.0"\nedition="2021"\n[lib]\nname="nested_game_logic"\ncrate-type=["cdylib"]\n[dependencies]\nnested-support={path="../support"}\ngodot={git="https://github.com/godot-rust/gdext",branch="master",default-features=false,features=["api-4-7"]}\n`);
    await writeFile(join(workspace, "support/Cargo.toml"), `[package]\nname="nested-support"\nversion="0.1.0"\nedition="2021"\n`);
    await writeFile(join(workspace, "Cargo.lock"), locked);
    await writeFile(join(project, "Cargo.lock"), "unselected root lockfile sentinel\n");
    await writeFile(join(project, sourcePath), rust);
    await writeFile(join(workspace, "support/src/lib.rs"), support);
    await writeFile(join(godotRoot, "addons/native/bin/native.gdextension"), `[configuration]\nentry_symbol="gdext_rust_init"\ncompatibility_minimum="4.7"\n[libraries]\nwindows.debug.x86_64="./active.dll"\nwindows.release.x86_64="./release.dll"\n`);
    await writeFile(join(project, libraryPath), "uninstalled working library sentinel\n");
    await writeFile(join(godotRoot, "project.godot"), `config_version=5\n[application]\nrun/main_scene="res://main.tscn"\n[input]\nadvance={"deadzone":0.2,"events":[]}\npause={"deadzone":0.2,"events":[]}\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n`);
    await writeFile(join(godotRoot, "main.gd"), script);
    await writeFile(join(godotRoot, "main.tscn"), `[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://main.gd" id="1"]\n[node name="WorkspaceMission" type="Node3D"]\nposition=Vector3(3,0,2)\nscript=ExtResource("1")\n[node name="WorkspaceState" type="WorkspaceState" parent="."]\n`);
    await writeFile(join(godotRoot, ".godot/cache-sentinel"), "working Godot cache\n");
    await writeFile(join(workspace, "target/cache-sentinel"), "working Cargo cache\n");
    await writeFile(join(root, "outside/Cargo.toml"), "[workspace]\nmembers=[]\n");
    await writeFile(join(root, "outside/preserved.txt"), "outside workspace sentinel\n");

    // CI provisions the full repository-pinned cache first; fixture setup stays offline.
    const generated = spawnSync("cargo", ["metadata", "--offline", "--format-version", "1"],
        {cwd: workspace, env: environment, windowsHide: true, encoding: "utf8", timeout: 60000, maxBuffer: 16 * 1024 * 1024});
    await writeFile(join(root, "fixture-metadata.log"), generated.stderr ?? "");
    assert.ifError(generated.error);
    assert.equal(generated.status, 0, generated.stderr);
    const lock = await readFile(join(workspace, "Cargo.lock"), "utf8"), pinnedIdentities = pinnedPackages(locked);
    check(lock.includes(`git+https://github.com/godot-rust/gdext?branch=master#${pinned[1]}`), "Fixture keeps the repository-pinned gdext revision");
    for (const [name, identity] of pinnedPackages(lock)) assert.equal(identity, pinnedIdentities.get(name), `Fixture preserves pinned dependency ${name}`);
    checks++;
    const metadata = JSON.parse(generated.stdout);
    check(metadata.packages.some(entry => entry.name === "aurum-workspace-game" && entry.targets.some(target => target.name === "nested_game_logic" && target.crate_types.includes("cdylib"))) && metadata.packages.some(entry => entry.name === "nested-support"), "Setup resolves the nested member and its local support dependency");
    const protectedPaths = [libraryPath, "Cargo.lock", "rust/Cargo.lock", "rust/Cargo.toml", "rust/game/Cargo.toml", "rust/support/Cargo.toml", "godot/main.gd", "godot/project.godot", "godot/addons/native/bin/native.gdextension"];
    const protectedHashes = new Map(await Promise.all(protectedPaths.map(async path => [path, hash(await readFile(join(project, path)))])));
    const initialCaches = {godot: await cacheInventory(join(godotRoot, ".godot")), cargo: await cacheInventory(join(workspace, "target"))};
    const workingPreserved = async (label, expectedSupport = support) => {
        for (const [path, expected] of protectedHashes) assert.equal(hash(await readFile(join(project, path))), expected, `${label}: ${path}`);
        assert.equal(await readFile(join(workspace, "support/src/lib.rs"), "utf8"), expectedSupport, `${label}: support member source`);
        assert.deepEqual(await cacheInventory(join(godotRoot, ".godot")), initialCaches.godot, `${label}: Godot cache`);
        assert.deepEqual(await cacheInventory(join(workspace, "target")), initialCaches.cargo, `${label}: Cargo cache`);
        checks++;
    };

    server = spawn(resolve(binary), ["studio", project, "--no-open", "--json", "--port", "0"], {env: environment, windowsHide: true});
    done = new Promise(resolve => { server.once("exit", code => resolve(code)); server.once("error", error => resolve(error)); });
    server.stderr.on("data", data => { stderr = (stderr + data).slice(-65536); });
    address = await bound(new Promise((resolve, reject) => {
        let output = "";
        server.once("error", reject);
        server.once("exit", code => reject(new Error(`Studio exited during startup: ${code}`)));
        server.stdout.on("data", data => {
            if (address) return;
            output = (output + data).slice(-65536);
            for (const line of output.split("\n")) if (line.startsWith("{")) { try { resolve(JSON.parse(line)); } catch {} }
        });
    }), 15000, "Studio startup");
    const url = new URL(address.url), token = url.searchParams.get("t");
    const request = async input => {
        const response = await fetch(`${url.origin}/api/project`, {method: "POST", headers: {"Content-Type": "application/json", "X-Aurum-Token": token}, body: JSON.stringify(input), signal: AbortSignal.timeout(330000)});
        return {status: response.status, value: await response.json()};
    };
    const action = async input => { const result = await request(input); assert.equal(result.status, 200, JSON.stringify(result.value)); return result.value; };
    const input = async name => { await action({op: "native_input", input_actions: [{action: name, pressed: true}]}); await action({op: "native_input", input_actions: [{action: name, pressed: false}]}); };
    const retained = async (pid, checkpoint, label) => {
        check((await action({op: "native_status"})).pid === pid, `${label} retains the accepted native PID`);
        assert.deepEqual((await action({op: "native_checkpoint"})).checkpoint, checkpoint, `${label} retains the accepted checkpoint`); checks++;
    };
    const revisionRequest = {changes: [
        {path: sourcePath, action: "replace", expected_sha256: hash(rust), text: changed},
        {path: "rust/support/src/lib.rs", action: "replace", expected_sha256: hash(support), text: changedSupport},
    ]};
    const started = await action({op: "native_start", rendered: false}); evidence.started = started;
    check(started.running === true && started.native_dependencies.compiled.package === "aurum-workspace-game" && started.native_dependencies.compiled.target === "nested_game_logic" && started.native_dependencies.compiled.offline === true && started.native_dependencies.compiled.lock_sha256 === hash(lock), "Contained workspace boots its selected offline member artifact using the nested lockfile");
    await input("advance"); await input("pause");
    const before = await action({op: "native_checkpoint"}); evidence.before = before.checkpoint;
    check(before.checkpoint.paused === true && before.checkpoint.custom.counter === 13, "Real Rust method invokes the local workspace dependency before checkpoint capture");
    await workingPreserved("Private native start");

    const externalEdit = rust + "\n// newer working edit\n";
    await writeFile(join(project, sourcePath), externalEdit);
    try {
        const stale = await action({op: "changes_apply", ...revisionRequest}); evidence.stale = stale;
        check(stale.ok === false && stale.applied === false && stale.phase === "preflight" && await readFile(join(project, sourcePath), "utf8") === externalEdit && await readFile(join(workspace, "support/src/lib.rs"), "utf8") === support, "Stale linked revision preserves the newer edit and never publishes the support change");
        await retained(started.pid, before.checkpoint, "Stale revision");
    } finally { await writeFile(join(project, sourcePath), rust); }
    const invalid = await action({op: "changes_validate", changes: [revisionRequest.changes[0], {...revisionRequest.changes[1], text: changedSupport + "\ninvalid Rust syntax!\n"}]}); evidence.invalid = invalid;
    check(invalid.ok === false && invalid.validated === false && invalid.applied === false && invalid.cleanup_ok === true && invalid.candidate_retained === false && await readFile(join(project, sourcePath), "utf8") === rust && await readFile(join(workspace, "support/src/lib.rs"), "utf8") === support, "Invalid support source refuses the entire linked candidate and preserves both working members");
    await retained(started.pid, before.checkpoint, "Invalid member-source candidate");
    const missing = await action({op: "changes_validate", changes: [revisionRequest.changes[0], {path: "rust/support/src/lib.rs", action: "delete", expected_sha256: hash(support)}]}); evidence.missing = missing;
    check(missing.ok === false && missing.validated === false && missing.applied === false && missing.cleanup_ok === true && await readFile(join(project, sourcePath), "utf8") === rust && await readFile(join(workspace, "support/src/lib.rs"), "utf8") === support, "Missing support source refuses the entire linked candidate and preserves both working members");
    await retained(started.pid, before.checkpoint, "Missing local-dependency source");
    check((await action({op: "changes_history"})).retained_revisions === 0, "Rejected workspace candidates create no source revision history");
    await workingPreserved("Rejected source candidates");

    const validated = await action({op: "changes_validate", ...revisionRequest}); evidence.validated = validated;
    const artifact = validated.native_artifacts?.[0], loaded = validated.native_dependencies?.libraries?.[0];
    check(validated.ok === true && validated.native_build === true && validated.files_changed === 2 && validated.applied === false && validated.native_artifacts_published === false && validated.source_still_current === true && validated.requested_changes_intact === true && validated.cleanup_ok === true && validated.candidate_retained === false, "Both linked workspace sources validate privately with intact proposals and completed cleanup");
    check(validated.native_artifacts.length === 1 && artifact.path === libraryPath && /^[a-f0-9]{64}$/.test(artifact.sha256) && validated.native_dependencies.ok === true && validated.native_dependencies.manifests === 1 && validated.native_dependencies.libraries.length === 1 && loaded.path === libraryPath && loaded.sha256 === artifact.sha256 && loaded.loaded === true && loaded.dependencies === 0, "Nested Godot root loads the exact compiled member library identified in the receipt");
    const published = await action({op: "changes_apply", ...revisionRequest}); evidence.published = published;
    check(published.ok === true && published.applied === true && published.native_build === true && published.files_changed === 2 && published.native_artifacts_published === false && published.cleanup_ok === true && await readFile(join(project, sourcePath), "utf8") === changed && await readFile(join(workspace, "support/src/lib.rs"), "utf8") === changedSupport, "Both linked member sources publish through one source-only revision");
    await retained(started.pid, before.checkpoint, "Source publication");
    const history = await action({op: "changes_history", revision_id: published.publication.revision_id}); evidence.history = history;
    const recorded = history.revisions[0];
    check(recorded.files_truncated === false && recorded.files.some(file => file.path === sourcePath && file.before_sha256 === hash(rust) && file.after_sha256 === hash(changed)) && recorded.files.some(file => file.path === "rust/support/src/lib.rs" && file.before_sha256 === hash(support) && file.after_sha256 === hash(changedSupport)) && recorded.files.every(file => revisionRequest.changes.some(change => change.path === file.path) || file.path.endsWith(".uid")), "One revision history records both linked source identities and excludes generated libraries");
    await workingPreserved("Source publication", changedSupport);

    const replaced = await action({op: "native_replace"}); evidence.replaced = replaced;
    check(replaced.state_preserved === true && replaced.pid !== started.pid, "Nested workspace native successor activates after verified checkpoint restoration");
    await retained(replaced.pid, before.checkpoint, "Native successor");
    const inspected = await action({op: "native_inspect", node_path: "WorkspaceState"});
    check(inspected.properties.find(property => property.name === "build_revision")?.value === 2, "Actual renamed member-library constructor executes the new implementation");
    const undone = await action({op: "changes_undo", revision_id: published.publication.revision_id}); evidence.undone = undone;
    check(undone.ok === true && await readFile(join(project, sourcePath), "utf8") === rust && await readFile(join(workspace, "support/src/lib.rs"), "utf8") === support, "Whole-revision undo restores both linked workspace sources");
    await retained(replaced.pid, before.checkpoint, "Source undo");
    await input("pause"); await input("advance"); await input("pause");
    const after = await action({op: "native_checkpoint"}); evidence.after = after.checkpoint;
    check(after.checkpoint.paused === true && after.checkpoint.custom.counter === 23, "Actual native method proves both the new game revision and new support logic remain active after source undo");
    await workingPreserved("Native replacement and source undo");

    evidence.hintRefusals = [];
    for (const hint of ["missing-workspace", workspace, "../outside"]) {
        await writeFile(join(project, "aurum.toml"), config(hint));
        try {
            const refused = await request({op: "native_replace"}); evidence.hintRefusals.push({hint, ...refused});
            check(refused.status === 400, "Missing, absolute or external workspace hints refuse native replacement");
            await retained(replaced.pid, after.checkpoint, "Workspace hint refusal");
        } finally { await writeFile(join(project, "aurum.toml"), originalConfig); }
    }
    const junction = join(project, "linked-rust");
    await symlink(join(root, "outside"), junction, "junction");
    try {
        check((await lstat(junction)).isSymbolicLink(), "Escape fixture is a real Windows directory junction");
        await writeFile(join(project, "aurum.toml"), config("linked-rust"));
        const refused = await request({op: "native_replace"}); evidence.hintRefusals.push({hint: "linked-rust", ...refused});
        check(refused.status === 400, "A workspace junction escape refuses native replacement");
        await retained(replaced.pid, after.checkpoint, "Workspace junction refusal");
    } finally { await writeFile(join(project, "aurum.toml"), originalConfig); await unlink(junction); }
    check(await readFile(join(root, "outside/preserved.txt"), "utf8") === "outside workspace sentinel\n" && await readFile(join(root, "outside/Cargo.toml"), "utf8") === "[workspace]\nmembers=[]\n", "Rejected workspace hints leave external sentinel content untouched");
    check(await readFile(join(project, "aurum.toml"), "utf8") === originalConfig && await readFile(join(project, sourcePath), "utf8") === rust, "Fixture restores the supported configuration and accepted working source");
    await workingPreserved("Hint refusals");
    await action({op: "native_stop"});
    evidence.ok = true;
} catch (error) {
    evidence.ok = false; evidence.error = error.stack ?? String(error);
    throw error;
} finally {
    if (address) { const url = new URL(address.url); try { await fetch(`${url.origin}/api/stop`, {method: "POST", headers: {"X-Aurum-Token": url.searchParams.get("t")}, signal: AbortSignal.timeout(5000)}); } catch {} }
    if (server) { try { await bound(done, 10000, "Studio shutdown"); } catch { server.kill(); await bound(done, 5000, "Studio termination"); } }
    evidence.checks = checks;
    await writeFile(join(root, "studio-stderr.log"), stderr);
    await writeFile(join(root, "verification.json"), JSON.stringify(evidence, null, 2));
}
console.log(JSON.stringify({checks, status: "passed", evidence: root}));
