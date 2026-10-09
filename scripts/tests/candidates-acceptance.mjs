// Actual Godot validation of disposable candidates through CLI, MCP and HTTP.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdir, mkdtemp, writeFile, readFile, readdir, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createHash } from "node:crypto";

const [binary, godot] = process.argv.slice(2);
assert.ok(binary && godot, "Usage: node candidates-acceptance.mjs <aurum> <godot>");
const executable = resolve(binary), runtime = resolve(godot);
const root = await mkdtemp(join(tmpdir(), "aurum-candidate-acceptance-"));
const project = join(root, "project"), sharedData = join(root, "shared-userdata");
await mkdir(project); await mkdir(sharedData);
await writeFile(join(sharedData, "keep.txt"), "unchanged");
const original = "extends Node\nvar value: int = 1\n";
const hash = text => createHash("sha256").update(text).digest("hex");
await writeFile(join(project, "aurum.toml"), 'schema_version=1\nname="candidate-fixture"\nmodules=[]\n');
await writeFile(join(project, "project.godot"), 'config_version=5\n[application]\nconfig/name="CandidateFixture"\nrun/main_scene="res://main.tscn"\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n');
await writeFile(join(project, "main.gd"), original);
await writeFile(join(project, "obsolete.gd"), "extends Node\n");
await writeFile(join(project, "main.tscn"), '[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://main.gd" id="1"]\n[node name="Main" type="Node"]\nscript=ExtResource("1")\n');
await writeFile(join(project, ".env"), "candidate-secret-sentinel");
const environment = { ...process.env, AURUM_STUDIO_HOME: join(root,"state"), AURUM_GODOT: runtime,
    APPDATA: sharedData, LOCALAPPDATA: sharedData, XDG_DATA_HOME: sharedData, XDG_CONFIG_HOME: sharedData, XDG_CACHE_HOME: sharedData };
const changes = [
    { path: "helper.gd", action: "create", expected_sha256: "", text: 'extends RefCounted\nconst VALUE: int = 7\nstatic func _static_init():\n\tif FileAccess.file_exists("res://.env"):\n\t\tpush_error("candidate secret was copied")\n\tvar f = FileAccess.open("user://candidate-probe.txt", FileAccess.WRITE)\n\tif f != null:\n\t\tf.store_string("private userdata")\n' },
    { path: "main.gd", action: "replace", expected_sha256: hash(original), text: 'extends Node\nconst Helper = preload("res://helper.gd")\nvar value: int = Helper.VALUE\n' },
    { path: "obsolete.gd", action: "delete", expected_sha256: hash("extends Node\n") },
];
let checks = 0;
const check = (condition, message) => { assert.ok(condition, message); checks++; };
const cli = (input, readOnly = false) => {
    const result = spawnSync(executable, ["project",project,"--request-json",JSON.stringify(input), ...(readOnly ? ["--read-only"] : [])], {
        env:environment, encoding:"utf8", windowsHide:true, timeout:180_000,
    });
    assert.ifError(result.error);
    return { code:result.status, value:JSON.parse(result.stdout.trim()) };
};
const unchanged = async () => {
    check(await readFile(join(project,"main.gd"),"utf8") === original,"working script unchanged");
    check(await readFile(join(project,"obsolete.gd"),"utf8") === "extends Node\n","working deletion not applied");
    const entries = await readdir(project);
    check(!entries.includes("helper.gd") && !entries.includes(".godot") && !entries.includes("main.gd.uid"),"working project has no candidate/import artifacts");
    check((await readdir(sharedData)).join(",") === "keep.txt" && await readFile(join(sharedData,"keep.txt"),"utf8") === "unchanged","standard userdata unchanged");
};
check(cli({op:"changes_validate",changes},true).code !== 0,"write permission required");
check(!(await readdir(project)).includes(".aurum"),"permission refusal creates no state");
const success = cli({op:"changes_validate",changes});
check(success.code === 0 && success.value.ok && success.value.validated,"real Godot linked-file candidate passes");
check(success.value.validation.resources_ok && success.value.validation.resources_checked >= 3,"actual resource validation receipt");
check(!success.value.applied && !success.value.gameplay_tested,"no publication/gameplay claim");
check(success.value.cleanup_ok && !success.value.candidate_retained,"private candidate removed");
await unchanged();
const failed = cli({op:"changes_validate",changes:[{path:"main.gd",action:"replace",expected_sha256:hash(original),text:"extends Node\nvar =\n"}]});
check(failed.code !== 0 && !failed.value.ok && !failed.value.validated,"real script parse failure refused");
check(failed.value.cleanup_ok && failed.value.validation.errors.length > 0,"failed candidate cleaned and diagnostics retained");
const last = cli({op:"changes_last"},true).value;
check(last.present && last.receipt.check_id === failed.value.check_id,"last completed receipt discoverable");
check((await stat(failed.value.evidence)).size < 64*1024,"retained receipt size bound");
await unchanged();

const done = child => new Promise((resolve,reject) => { child.once("error",reject); child.once("exit",code=>resolve(code)); });
const bounded = (promise,ms,label) => new Promise((resolve,reject) => {
    const timer = setTimeout(()=>reject(new Error(`${label} deadline exceeded`)),ms);
    promise.then(value=>{ clearTimeout(timer); resolve(value); },error=>{ clearTimeout(timer); reject(error); });
});
const mcp = spawn(executable,["mcp","--root",project,"--tools","studio"],{env:environment,windowsHide:true});
const mcpDone = done(mcp);
let output = "";
mcp.stdout.on("data",data=>{ output+=data; });
mcp.stderr.on("data",()=>{});
for (const request of [
    {jsonrpc:"2.0",id:1,method:"initialize",params:{protocolVersion:"2025-06-18",clientInfo:{name:"candidate-fixture",version:"1"},capabilities:{}}},
    {jsonrpc:"2.0",id:2,method:"tools/call",params:{name:"aurum_project_action",arguments:{op:"changes_validate",changes}}},
]) mcp.stdin.write(`${JSON.stringify(request)}\n`);
mcp.stdin.end();
check(await bounded(mcpDone,180_000,"MCP candidate") === 0,"MCP process completed");
const responses = output.trim().split("\n").map(line=>JSON.parse(line));
check(responses[1].result.structuredContent.ok && responses[1].result.structuredContent.validated,"actual MCP candidate validation");
await unchanged();

const server = spawn(executable,["studio",project,"--json","--port","0"],{env:environment,windowsHide:true});
const serverDone = done(server);
let address, buffer = "";
try {
    address = await bounded(new Promise((resolve,reject)=>{
        server.once("error",reject);
        server.stdout.on("data",data=>{ buffer+=data; const line=buffer.split("\n").find(line=>line.startsWith("{")); if(line){ try{resolve(JSON.parse(line));}catch{} } });
    }),15_000,"Studio startup");
    const url = new URL(address.url), token = url.searchParams.get("t");
    const response = await fetch(`${url.origin}/api/project`,{method:"POST",headers:{"X-Aurum-Token":token,"Content-Type":"application/json"},body:JSON.stringify({op:"changes_validate",changes}),signal:AbortSignal.timeout(180_000)});
    const receipt = await response.json();
    check(response.status === 200 && receipt.ok && receipt.validated,"actual HTTP candidate validation");
    await unchanged();
    const applied = cli({op:"changes_apply",changes});
    check(applied.code === 0 && applied.value.ok && applied.value.applied,"validated revision published through CLI");
    check(await readFile(join(project,"main.gd"),"utf8") === changes[1].text && (await readdir(project)).includes("helper.gd") && !(await readdir(project)).includes("obsolete.gd"),"linked changes published together");
    const revision = applied.value.publication.revision_id;
    const undone = cli({op:"changes_undo",revision_id:revision});
    check(undone.code === 0 && undone.value.ok,"whole revision undo");
    await unchanged();
    const forget = cli({op:"changes_forget",revision_id:revision});
    check(forget.code === 0 && forget.value.undo_available === false && forget.value.content_changed === false,"completed history cleanup does not change source");
    await writeFile(join(root,"verification.json"),JSON.stringify({checks,root,runtime,success:success.value,failure:failed.value,http:receipt,applied:applied.value,undone:undone.value},null,2));
} finally {
    if(address){const url=new URL(address.url);try{await fetch(`${url.origin}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":url.searchParams.get("t")},signal:AbortSignal.timeout(5_000)});}catch{}}
    try{await bounded(serverDone,10_000,"Studio shutdown");}catch{server.kill();await bounded(serverDone,5_000,"Studio termination");}
}
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
