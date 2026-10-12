// Actual Godot validation of disposable candidates through CLI, MCP and HTTP.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdir, mkdtemp, writeFile, readFile, readdir, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createHash } from "node:crypto";

const [binary, godot] = process.argv.slice(2);
assert.ok(binary && godot, "Usage: node candidates-acceptance.mjs <aurum> <godot>");
const executable = resolve(binary), runtime = resolve(godot);
const root = await mkdtemp(join(tmpdir(), "aurum-candidate-acceptance-"));
const project = join(root, "project"), sharedData = join(root, "shared-userdata");
const temporary = join(root, "temporary");
await mkdir(project); await mkdir(sharedData); await mkdir(temporary);
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
    TEMP: temporary, TMP: temporary,
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
check(cli({op:"changes_history"},true).value.retained_revisions === 0,"read-only empty revision history");
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
    {jsonrpc:"2.0",id:3,method:"tools/call",params:{name:"aurum_project_action",arguments:{op:"changes_apply",changes}}},
    {jsonrpc:"2.0",id:4,method:"tools/call",params:{name:"aurum_project_query",arguments:{op:"changes_history"}}},
]) mcp.stdin.write(`${JSON.stringify(request)}\n`);
mcp.stdin.end();
check(await bounded(mcpDone,180_000,"MCP candidate") === 0,"MCP process completed");
const responses = output.trim().split("\n").map(line=>JSON.parse(line));
check(responses[1].result.structuredContent.ok && responses[1].result.structuredContent.validated,"actual MCP candidate validation");
const mcpApplied = responses[2].result.structuredContent;
check(mcpApplied.ok && mcpApplied.applied,"actual MCP journaled publication");
check(responses[3].result.structuredContent.revisions.some(row=>row.revision_id === mcpApplied.publication.revision_id),"read-only MCP history discovers the published undo identity");
check(cli({op:"changes_undo",revision_id:mcpApplied.publication.revision_id}).value.ok,"MCP publication can be undone through shared CLI backend");
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
    const httpAction = async input => {
        const response = await fetch(`${url.origin}/api/project`,{method:"POST",headers:{"X-Aurum-Token":token,"Content-Type":"application/json"},body:JSON.stringify(input),signal:AbortSignal.timeout(180_000)});
        return {status:response.status,value:await response.json()};
    };
    const httpApplied = await httpAction({op:"changes_apply",changes});
    check(httpApplied.status === 200 && httpApplied.value.ok && httpApplied.value.applied,"actual HTTP journaled publication");
    const httpHistory = await httpAction({op:"changes_history",revision_id:httpApplied.value.publication.revision_id,limit:1});
    check(httpHistory.status === 200 && httpHistory.value.revisions[0].files.length === 1 && httpHistory.value.revisions[0].files_truncated,"actual HTTP bounded history detail");
    const httpUndone = await httpAction({op:"changes_undo",revision_id:httpApplied.value.publication.revision_id});
    check(httpUndone.status === 200 && httpUndone.value.ok,"actual HTTP whole-revision undo");
    await unchanged();
    const applied = cli({op:"changes_apply",changes});
    check(applied.code === 0 && applied.value.ok && applied.value.applied,"validated revision published through CLI");
    check(await readFile(join(project,"main.gd"),"utf8") === changes[1].text && (await readdir(project)).includes("helper.gd") && !(await readdir(project)).includes("obsolete.gd"),"linked changes published together");
    const revision = applied.value.publication.revision_id;
    const history = cli({op:"changes_history",revision_id:revision},true).value;
    check(history.revisions[0].files.length >= 3 && !history.source_state_checked && !history.undo_preconditions_checked,"actual CLI history returns recorded hashes, not live undo proof");
    const undone = cli({op:"changes_undo",revision_id:revision});
    check(undone.code === 0 && undone.value.ok,"whole revision undo");
    await unchanged();
    const forget = cli({op:"changes_forget",revision_id:revision});
    check(forget.code === 0 && forget.value.undo_available === false && forget.value.content_changed === false,"completed history cleanup does not change source");
    check(!cli({op:"changes_history"},true).value.revisions.some(row=>row.revision_id === revision),"forgotten revision no longer appears in history");
    await writeFile(join(root,"verification.json"),JSON.stringify({checks,root,runtime,success:success.value,failure:failed.value,http:receipt,mcpApplied,httpApplied,httpUndone,applied:applied.value,undone:undone.value},null,2));
} finally {
    if(address){const url=new URL(address.url);try{await fetch(`${url.origin}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":url.searchParams.get("t")},signal:AbortSignal.timeout(5_000)});}catch{}}
    try{await bounded(serverDone,10_000,"Studio shutdown");}catch{server.kill();await bounded(serverDone,5_000,"Studio termination");}
}
// Kill the actual Aurum host while Godot is executing trusted fixture code.
// The OS job must terminate that worker; its recognized copy is reclaimed on
// the next allocation, not by deleting arbitrary old temporary directories.
const probePath = join(root,"crash-probe.json");
const hang = spawn(executable,["project",project,"--request-json",JSON.stringify({op:"changes_validate",changes:[
    {path:"main.gd",action:"replace",expected_sha256:hash(original),text:'extends Node\nconst Probe = preload("res://crash_probe.gd")\n'},
    {path:"crash_probe.gd",action:"create",expected_sha256:"",text:`extends RefCounted\nstatic func _static_init():\n\tvar f = FileAccess.open(${JSON.stringify(probePath.replaceAll("\\","/"))}, FileAccess.WRITE)\n\tf.store_string(JSON.stringify({"pid": OS.get_process_id(), "userdata": OS.get_environment("APPDATA")}))\n\tf.close()\n\tOS.delay_msec(30000)\n`},
]})],{env:environment,windowsHide:true});
const hangDone = done(hang);
hang.stdout.on("data",()=>{}); hang.stderr.on("data",()=>{});
let probe;
let hangLog = "";
hang.stdout.on("data",data=>{hangLog=(hangLog+data).slice(-8192);});
hang.stderr.on("data",data=>{hangLog=(hangLog+data).slice(-8192);});
try {
    probe = await (async()=>{
        const deadline = Date.now()+30_000;
        while(Date.now()<deadline) {
            try { return JSON.parse(await readFile(probePath,"utf8")); } catch {}
            await new Promise(resolve=>setTimeout(resolve,50));
        }
        throw new Error(`Godot crash fixture did not become ready: ${hangLog}`);
    })();
    const alive = pid => { try { process.kill(pid,0); return true; } catch(error) { if(error.code === "ESRCH") return false; throw error; } };
    check(alive(probe.pid),"actual candidate Godot worker was running before host crash");
    hang.kill(); await bounded(hangDone,10_000,"candidate host termination");
    const deadline = Date.now()+5_000;
    while(alive(probe.pid) && Date.now()<deadline) await new Promise(resolve=>setTimeout(resolve,25));
    check(!alive(probe.pid),"actual Godot worker cannot survive host crash");
    const abandoned = dirname(probe.userdata);
    check(abandoned.startsWith(join(temporary,"aurum-candidates-v1")),"crashed candidate belongs to the private fixture pool");
    check((await stat(abandoned)).isDirectory(),"host crash leaves a recognized candidate for recovery");
    const recovery = cli({op:"changes_validate",changes});
    check(recovery.value.ok && recovery.value.workspace_recovered === 1,"next validation reclaims the abandoned copy and runs successfully");
    let exists = true; try { await stat(abandoned); } catch(error) { if(error.code === "ENOENT") exists = false; else throw error; }
    check(!exists,"crashed private copy removed");
    await unchanged();
    await writeFile(join(root,"crash-verification.json"),JSON.stringify({checks,probe,recovery:recovery.value},null,2));
} finally {
    if(hang.exitCode === null && hang.signalCode === null) { hang.kill(); await bounded(hangDone,5_000,"fixture final termination"); }
}
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
