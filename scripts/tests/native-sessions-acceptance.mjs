// Actual persistent HTTP/MCP native sessions, not a simulated game backend.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, mkdir, writeFile, readFile, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const [binary,godot] = process.argv.slice(2);
assert.ok(binary && godot,"Usage: native-sessions-acceptance.mjs <aurum> <godot>");
const root = await mkdtemp(join(tmpdir(),"aurum-native-acceptance-"));
const project = join(root,"project"); await mkdir(project);
const script = 'extends Node3D\n@export var speed: float = 4.0\nvar inventory = ["cell", "tool"]\nvar enemies = ["guard-4", "guard-9"]\nvar cooldowns = {"dash":1.5}\nvar mission_step = 3\nfunc aurum_capture_state():\n\treturn {"schema":1,"position":position,"inventory":inventory,"enemies":enemies,"cooldowns":cooldowns,"mission_step":mission_step}\nfunc aurum_restore_state(state):\n\tif not state is Dictionary or state.get("schema") != 1 or not state.get("position") is Vector3: return false\n\tposition = state.position\n\tinventory = state.inventory.duplicate()\n\tenemies = state.enemies.duplicate()\n\tcooldowns = state.cooldowns.duplicate()\n\tmission_step = int(state.mission_step)\n\treturn true\n';
await writeFile(join(project,"aurum.toml"),'schema_version=1\nname="native-acceptance"\nmodules=[]\n');
await writeFile(join(project,"project.godot"),'config_version=5\n[application]\nrun/main_scene="res://main.tscn"\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n');
await writeFile(join(project,"main.gd"),script);
await writeFile(join(project,"main.tscn"),'[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://main.gd" id="1"]\n[node name="Mission" type="Node3D"]\nscript=ExtResource("1")\n');
const environment = {...process.env,AURUM_GODOT:resolve(godot),AURUM_STUDIO_HOME:join(root,"state"),APPDATA:join(root,"userdata"),LOCALAPPDATA:join(root,"userdata")};
let checks = 0;
const check = (value,message) => {assert.ok(value,message);checks++;};
const bounded = (promise,ms,label) => new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>reject(new Error(`${label} deadline exceeded`)),ms);
    promise.then(value=>{clearTimeout(timer);resolve(value);},error=>{clearTimeout(timer);reject(error);});
});
const done = child => new Promise((resolve,reject)=>{child.once("error",reject);child.once("exit",code=>resolve(code));});
const server=spawn(resolve(binary),["studio",project,"--no-open","--json","--port","0"],{env:environment,windowsHide:true});
const serverDone=done(server);let address, output="";
server.stderr.on("data",()=>{});
try {
    address=await bounded(new Promise((resolve,reject)=>{server.once("error",reject);server.stdout.on("data",data=>{
        output+=data;const line=output.split("\n").find(line=>line.startsWith("{"));if(line){try{resolve(JSON.parse(line));}catch{}}
    });}),15000,"Studio startup");
    const url=new URL(address.url), token=url.searchParams.get("t");
    const action=async input=>{
        const response=await fetch(`${url.origin}/api/project`,{method:"POST",headers:{"X-Aurum-Token":token,"Content-Type":"application/json"},body:JSON.stringify(input),signal:AbortSignal.timeout(180000)});
        return {status:response.status,value:await response.json()};
    };
    check((await action({op:"native_status"})).value.running === false,"native status starts empty");
    const started=await action({op:"native_start",rendered:false});
    check(started.status === 200 && started.value.running && started.value.source_private,"actual persistent HTTP native start");
    check((await readFile(join(project,"main.gd"),"utf8")) === script && !(await readdir(project)).includes(".godot"),"native preparation preserves original source/import state");
    const initialPid=started.value.pid;
    const savedData=await action({op:"native_userdata_list"});
    check(savedData.value.generations.length === 1 && savedData.value.current,"native saved-data generation is discoverable");
    check((await action({op:"native_userdata_forget",userdata_id:savedData.value.current})).status === 400,"current native saved data cannot be deleted");
    const duplicate=await action({op:"native_start"});
    check(duplicate.status === 400,"duplicate native start refused");
    const applied=await action({op:"native_apply",properties:[{path:".",property:"position",value:{__aurum_type:"Vector3",value:[5,2,-8]}}]});
    check(applied.value.ok,"actual native live property change");
    const checkpoint=(await action({op:"native_checkpoint"})).value;
    check(checkpoint.ok && checkpoint.complete,"complete native mission checkpoint");
    await writeFile(join(project,"main.gd"),script.replace("return true\n","return false\n"));
    const rejected=await action({op:"native_replace"});
    check(rejected.status === 400,"rejected restore refuses native replacement");
    const retained=await action({op:"native_status"});
    check(retained.value.running && retained.value.pid === initialPid,"rejected replacement retains original PID");
    const afterReject=(await action({op:"native_checkpoint"})).value;
    assert.deepEqual(afterReject.checkpoint.custom,checkpoint.checkpoint.custom);checks++;
    check(!afterReject.checkpoint.paused,"failed replacement unfreezes original");
    await writeFile(join(project,"main.gd"),script.replace("speed: float = 4.0","speed: float = 6.0"));
    const replacement=await action({op:"native_replace"});
    check(replacement.value.ok && replacement.value.state_preserved && replacement.value.pid !== initialPid,"actual HTTP compatible native replacement");
    const after=(await action({op:"native_checkpoint"})).value;
    assert.deepEqual(after.checkpoint.custom,checkpoint.checkpoint.custom);checks++;
    const inspected=(await action({op:"native_inspect"})).value;
    check(inspected.properties.find(property=>property.name === "speed").value === 6,"new code is active after handoff");
    check((await action({op:"native_stop"})).value.stopped,"owned native stop");
    check((await action({op:"native_status"})).value.running === false,"native stop reflected in status");
    const retainedData=(await action({op:"native_userdata_list"})).value;
    check(retainedData.current && retainedData.generations.length >= 2,"native stop retains current saved-data generation");
    await writeFile(join(root,"http-evidence.json"),JSON.stringify({checks,started:started.value,replacement:replacement.value,before:checkpoint,after},null,2));
} finally {
    if(address){const url=new URL(address.url);try{await fetch(`${url.origin}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":url.searchParams.get("t")},signal:AbortSignal.timeout(5000)});}catch{}}
    try{await bounded(serverDone,10000,"Studio shutdown");}catch{server.kill();await bounded(serverDone,5000,"Studio termination");}
}
// A persistent MCP connection keeps the same owned game between tool calls.
const mcp=spawn(resolve(binary),["mcp","--root",project,"--tools","studio"],{env:environment,windowsHide:true});
const mcpDone=done(mcp);mcp.stderr.on("data",()=>{});
let buffered="", nextId=1;const pending=new Map();
mcp.stdout.on("data",data=>{
    buffered+=data;
    for(;;){const end=buffered.indexOf("\n");if(end<0)break;const line=buffered.slice(0,end);buffered=buffered.slice(end+1);
        const message=JSON.parse(line);const resolve=pending.get(message.id);if(resolve){pending.delete(message.id);resolve(message);}}
});
const rpc=async(method,params)=>{const id=nextId++;const result=new Promise(resolve=>pending.set(id,resolve));mcp.stdin.write(`${JSON.stringify({jsonrpc:"2.0",id,method,params})}\n`);return bounded(result,180000,"MCP native operation");};
const tool=async(input,query=false)=>(await rpc("tools/call",{name:query?"aurum_project_query":"aurum_project_action",arguments:input})).result;
try {
    await rpc("initialize",{protocolVersion:"2025-06-18",clientInfo:{name:"native-acceptance",version:"1"},capabilities:{}});
    check((await tool({op:"native_start",rendered:false})).structuredContent.running,"actual MCP native start");
    check((await tool({op:"native_inspect"},true)).structuredContent.ok,"read-only MCP native inspection");
    check((await tool({op:"native_replace"},true)).isError,"read-only MCP query cannot replace a native process");
    check((await tool({op:"native_stop"})).structuredContent.stopped,"actual MCP owned native stop");
} finally {
    mcp.stdin.end();try{await bounded(mcpDone,10000,"MCP shutdown");}catch{mcp.kill();await bounded(mcpDone,5000,"MCP termination");}
    const stopped=spawnSync(resolve(binary),["project",project,"--request-json",JSON.stringify({op:"native_host_stop"})],{env:environment,windowsHide:true,encoding:"utf8",timeout:15000});
    assert.ifError(stopped.error);
    assert.equal(stopped.status,0,"shared controller cleanup must succeed");
}
await writeFile(join(root,"verification.json"),JSON.stringify({ok:true,checks,root,scope:"actual native HTTP/MCP lifecycle and state restoration"},null,2));
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
