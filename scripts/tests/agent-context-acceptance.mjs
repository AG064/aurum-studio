// Real shared CLI/MCP/HTTP context contracts in a disposable source project.
import assert from "node:assert/strict";
import {spawn,spawnSync} from "node:child_process";
import {mkdtemp,mkdir,writeFile,readFile,unlink} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join,resolve} from "node:path";
const [binary]=process.argv.slice(2);assert.ok(binary,"Usage: agent-context-acceptance.mjs <aurum>");
const root=await mkdtemp(join(tmpdir(),"aurum-context-acceptance-")),project=join(root,"project");await mkdir(project);await mkdir(join(project,"game"));
await writeFile(join(project,"aurum.toml"),'schema_version=1\nname="context"\nmodules=[]\n');
await writeFile(join(project,"game/main.gd"),"one\n");await writeFile(join(project,"game/old.gd"),"keep\n");
await writeFile(join(project,".env"),"PRIVATE_CONTEXT_FIXTURE_SECRET");
const environment={...process.env,AURUM_STUDIO_HOME:join(root,"state")};
const cli=(input,readOnly=false)=>{const result=spawnSync(resolve(binary),["project",project,"--request-json",JSON.stringify(input),...(readOnly?["--read-only"]:[])],{env:environment,windowsHide:true,encoding:"utf8",timeout:20000});assert.ifError(result.error);return {code:result.status,value:JSON.parse(result.stdout)};};
let checks=0;const check=(value,message)=>{assert.ok(value,message);checks++;};
const bound=(promise,ms,label)=>new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error(`${label} deadline exceeded`)),ms);promise.then(value=>{clearTimeout(timer);resolve(value);},error=>{clearTimeout(timer);reject(error);});});
const scope={prefix:"game",extensions:["GD"]};
check(cli({op:"context_snapshot",...scope},true).code!==0,"read-only CLI cannot create a baseline");
const baseline=cli({op:"context_snapshot",...scope}).value;check(baseline.files===2 && baseline.baseline_id.length===64,"CLI captures scoped hashes, not content");
check(!JSON.stringify(baseline).includes("one\\n") && !JSON.stringify(baseline).includes("SECRET"),"snapshot output omits source and private data");
check(cli({op:"context_snapshot",...scope}).value.reused,"identical context reuses a baseline");
await writeFile(join(project,"game/main.gd"),"two\n");await unlink(join(project,"game/old.gd"));await writeFile(join(project,"game/new.gd"),"new\n");await writeFile(join(project,"game/texture.bin"),new Uint8Array([0,255]));
const first=cli({op:"context_delta",baseline_id:baseline.baseline_id,limit:1},true).value;
check(first.changed_files===3 && first.truncated && first.changes.length===1,"bounded CLI delta detects same-size edit, create and delete");
check(cli({op:"context_delta",baseline_id:baseline.baseline_id,after:first.next_after},true).code!==0,"pagination requires current content identity");
check(cli({op:"context_forget",baseline_id:baseline.baseline_id},true).code!==0,"read-only CLI cannot remove a baseline");
const server=spawn(resolve(binary),["studio",project,"--no-open","--json","--port","0"],{env:environment,windowsHide:true});
const serverDone=new Promise((resolve,reject)=>{server.once("error",reject);server.once("exit",resolve);});server.stderr.on("data",()=>{});
const mcp=spawn(resolve(binary),["mcp","--root",project,"--tools","studio"],{env:environment,windowsHide:true});
const mcpDone=new Promise((resolve,reject)=>{mcp.once("error",reject);mcp.once("exit",resolve);});mcp.stderr.on("data",()=>{});
let buffer="",id=1,address,serverOutput="";const pending=new Map();
mcp.stdout.on("data",data=>{buffer+=data;for(;;){const end=buffer.indexOf("\n");if(end<0)break;const value=JSON.parse(buffer.slice(0,end));buffer=buffer.slice(end+1);const finish=pending.get(value.id);if(finish){pending.delete(value.id);finish(value);}}});
const rpc=(method,params)=>{const current=id++,result=new Promise(resolve=>pending.set(current,resolve));mcp.stdin.write(`${JSON.stringify({jsonrpc:"2.0",id:current,method,params})}\n`);return bound(result,20000,"MCP context call");};
const tool=async(input,query=true)=>(await rpc("tools/call",{name:query?"aurum_project_query":"aurum_project_action",arguments:input})).result;
try {
    address=await bound(new Promise((resolve,reject)=>{server.once("error",reject);server.stdout.on("data",data=>{serverOutput+=data;const line=serverOutput.split("\n").find(line=>line.startsWith("{"));if(line){try{resolve(JSON.parse(line));}catch{}}});}),15000,"Studio startup");
    const url=new URL(address.url),token=url.searchParams.get("t");
    const http=async input=>{const response=await fetch(`${url.origin}/api/project`,{method:"POST",headers:{"Content-Type":"application/json","X-Aurum-Token":token},body:JSON.stringify(input),signal:AbortSignal.timeout(20000)});return {status:response.status,value:await response.json()};};
    const page=await http({op:"context_delta",baseline_id:baseline.baseline_id,after:first.next_after,current_id:first.current_id,limit:1});
    check(page.status===200 && page.value.changes[0].change==="created","HTTP continues the same CLI context page");
    await rpc("initialize",{protocolVersion:"2025-06-18",clientInfo:{name:"context-acceptance",version:"1"},capabilities:{}});
    const result=await tool({op:"context_delta",baseline_id:baseline.baseline_id});
    check(result.structuredContent.changed_files===3 && result.structuredContent.current_id===first.current_id,"MCP reads the same retained baseline/content");
    check((await tool({op:"context_snapshot",...scope})).isError,"read-only MCP refuses baseline creation");
    const current=await tool({op:"context_snapshot",...scope},false);check(current.structuredContent.files===2,"MCP action advances context explicitly");
    check(cli({op:"context_delta",baseline_id:current.structuredContent.baseline_id},true).value.changed_files===0,"CLI can reconnect using MCP's new baseline");
    await writeFile(join(project,"game/new.gd"),"changed\n");
    check((await http({op:"context_delta",baseline_id:baseline.baseline_id,after:first.next_after,current_id:first.current_id})).status===400,"source drift safely refuses mixed-page deltas");
    await writeFile(join(project,"aurum.toml"),"invalid configuration");
    check(cli({op:"context_delta",baseline_id:baseline.baseline_id},true).value.changed_files===3,"read-only delta remains available when project configuration is broken");
    await writeFile(join(project,"aurum.toml"),'schema_version=1\nname="context"\nmodules=[]\n');
    check((await http({op:"context_forget",baseline_id:baseline.baseline_id})).value.forgotten===baseline.baseline_id,"HTTP explicitly forgets only selected metadata");
    check((await readFile(join(project,"game/main.gd"),"utf8"))==="two\n","context operations preserve working source");
    check(cli({op:"context_delta",baseline_id:baseline.baseline_id},true).code!==0,"forgotten baseline requires a fresh snapshot");
    check((await tool({op:"describe",operation:"context_delta"})).structuredContent.read_only,"MCP advertises actual read-only context contract");
    await writeFile(join(root,"verification.json"),JSON.stringify({ok:true,checks,scope,baseline_id:baseline.baseline_id,current_id:first.current_id,interfaces:["CLI","HTTP","MCP"],claims:"bounded hash/path context, not measured model-token savings"},null,2));
} finally {
    mcp.stdin.end();try{await bound(mcpDone,10000,"MCP shutdown");}catch{mcp.kill();await bound(mcpDone,5000,"MCP termination");}
    if(address){const url=new URL(address.url);try{await fetch(`${url.origin}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":url.searchParams.get("t")},signal:AbortSignal.timeout(5000)});}catch{}}
    try{await bound(serverDone,10000,"Studio shutdown");}catch{server.kill();await bound(serverDone,5000,"Studio termination");}
}
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
