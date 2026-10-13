import assert from "node:assert/strict";
import {spawn,spawnSync} from "node:child_process";
import {mkdtemp,mkdir,writeFile,readFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join,resolve} from "node:path";
const [binary,godot]=process.argv.slice(2);assert.ok(binary && godot,"Usage: native-routing-acceptance.mjs <aurum> <godot>");
const root=await mkdtemp(join(tmpdir(),"aurum-native-routing-")),project=join(root,"project");await mkdir(project);
await writeFile(join(project,"aurum.toml"),'schema_version=1\nname="routing"\nmodules=[]\n');
await writeFile(join(project,"project.godot"),'config_version=5\n[application]\nrun/main_scene="res://main.tscn"\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n');
await writeFile(join(project,"main.gd"),'extends Node3D\n@export var speed: float = 4.0\n');
await writeFile(join(project,"main.tscn"),'[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://main.gd" id="1"]\n[node name="Mission" type="Node3D"]\nscript=ExtResource("1")\n');
const environment={...process.env,AURUM_GODOT:resolve(godot),AURUM_STUDIO_HOME:join(root,"state"),APPDATA:join(root,"user"),LOCALAPPDATA:join(root,"user")};
const cli=(input,readOnly=false)=>{const result=spawnSync(resolve(binary),["project",project,"--request-json",JSON.stringify(input),...(readOnly?["--read-only"]:[])],{env:environment,windowsHide:true,encoding:"utf8",timeout:240000});assert.ifError(result.error);return {code:result.status,value:JSON.parse(result.stdout)};};
const bound=(promise,ms,label)=>new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error(`${label} deadline exceeded`)),ms);promise.then(value=>{clearTimeout(timer);resolve(value);},error=>{clearTimeout(timer);reject(error);});});
let checks=0;const check=(value,message)=>{assert.ok(value,message);checks++;};
const clients=[];
function mcp(){
    const child=spawn(resolve(binary),["mcp","--root",project,"--tools","studio"],{env:environment,windowsHide:true});child.stderr.on("data",()=>{});
    const done=new Promise((resolve,reject)=>{child.once("error",reject);child.once("exit",resolve);});let buffer="",id=1;const pending=new Map();
    child.stdout.on("data",data=>{buffer+=data;for(;;){const end=buffer.indexOf("\n");if(end<0)break;const message=JSON.parse(buffer.slice(0,end));buffer=buffer.slice(end+1);const resolve=pending.get(message.id);if(resolve){pending.delete(message.id);resolve(message);}}});
    const rpc=(method,params)=>{const current=id++;const response=new Promise(resolve=>pending.set(current,resolve));child.stdin.write(`${JSON.stringify({jsonrpc:"2.0",id:current,method,params})}\n`);return bound(response,30000,"MCP call");};
    const client={child,done,rpc,call:async(input,query=true)=>(await rpc("tools/call",{name:query?"aurum_project_query":"aurum_project_action",arguments:input})).result};clients.push(client);return client;
}
let host;
try {
    check(cli({op:"native_status"},true).value.host_available===false,"read-only status does not start a controller");
    check(cli({op:"native_start"},true).code!==0,"read-only native start refused before launch");
    const started=cli({op:"native_start",rendered:false});check(started.code===0 && started.value.running,"CLI starts shared controller headlessly");
    host=JSON.parse(await readFile(join(project,".aurum/native/host.json"),"utf8"));
    const token=await readFile(join(project,".aurum/native/host.token"),"utf8");
    check(!JSON.stringify(host).includes(token),"host descriptor contains no credential");
    const http=async input=>{const response=await fetch(`http://127.0.0.1:${host.port}/api/project`,{method:"POST",headers:{"Content-Type":"application/json","X-Aurum-Token":token},body:JSON.stringify(input),signal:AbortSignal.timeout(15000)});return response.json();};
    check((await http({op:"native_status"})).pid===started.value.pid,"HTTP and CLI see same native PID");
    const first=mcp();await first.rpc("initialize",{protocolVersion:"2025-06-18",clientInfo:{name:"routing",version:"1"},capabilities:{}});
    check((await first.call({op:"native_status"})).structuredContent.pid===started.value.pid,"MCP shares CLI game");
    check((await first.call({op:"native_apply",properties:[]})).isError,"read-only MCP cannot mutate");
    check((await first.call({op:"native_apply",properties:[{path:".",property:"speed",value:7}]},false)).structuredContent.ok,"MCP mutates the shared native instance");
    const inspected=cli({op:"native_inspect"},true).value;
    check(inspected.properties.find(value=>value.name==="speed").value===7,"CLI reads MCP change without restart");
    first.child.stdin.end();await bound(first.done,10000,"MCP disconnect");
    check(cli({op:"native_status"},true).value.pid===started.value.pid,"MCP disconnect keeps canonical game alive");
    const second=mcp();await second.rpc("initialize",{protocolVersion:"2025-06-18",clientInfo:{name:"reconnect",version:"1"},capabilities:{}});
    check((await second.call({op:"native_status"})).structuredContent.pid===started.value.pid,"MCP reconnect resumes same native instance");
    const reused=spawnSync(resolve(binary),["studio",project,"--no-open","--json"],{env:environment,windowsHide:true,encoding:"utf8",timeout:15000});
    assert.ifError(reused.error);check(reused.status===0 && JSON.parse(reused.stdout).reused,"Studio invocation reuses existing controller");
    const alternateTemp=join(root,"alternate-temp");await mkdir(alternateTemp);
    const alternate=spawnSync(resolve(binary),["studio",project,"--no-open","--json"],{env:{...environment,TEMP:alternateTemp,TMP:alternateTemp},windowsHide:true,encoding:"utf8",timeout:15000});
    assert.ifError(alternate.error);check(alternate.status===0 && JSON.parse(alternate.stdout).reused && (await http({op:"native_status"})).pid===started.value.pid,"different temporary environments share the same controller");
    check(cli({op:"native_stop"}).value.stopped,"CLI stops the shared owned game");
    check((await http({op:"native_status"})).running===false,"HTTP observes CLI stop");
    check(cli({op:"native_host_stop"}).value.stopped,"CLI explicitly shuts down the shared controller");
    await writeFile(join(root,"verification.json"),JSON.stringify({ok:true,checks,root,pid:started.value.pid,host_pid:host.owner.pid,scope:"actual shared CLI/MCP/HTTP native controller"},null,2));
} finally {
    for(const client of clients){if(client.child.exitCode!==null || client.child.signalCode!==null)continue;client.child.stdin.end();try{await bound(client.done,10000,"client shutdown");}catch{client.child.kill();await bound(client.done,5000,"client termination");}}
    try{if(!host)host=JSON.parse(await readFile(join(project,".aurum/native/host.json"),"utf8"));const token=await readFile(join(project,".aurum/native/host.token"),"utf8");await fetch(`http://127.0.0.1:${host.port}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":token},signal:AbortSignal.timeout(5000)});}catch{}
}
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
