// Real Relay Yard gameplay and native replacement, using normal action input.
import assert from "node:assert/strict";
import {spawn} from "node:child_process";
import {mkdtemp,cp,readFile,writeFile} from "node:fs/promises";
import {tmpdir} from "node:os";
import {resolve,join,basename,dirname} from "node:path";
import {fileURLToPath} from "node:url";
const [binary,godot]=process.argv.slice(2);
assert.ok(binary && godot,"Usage: native-mission-acceptance.mjs <aurum> <godot>");
const repository=resolve(dirname(fileURLToPath(import.meta.url)),"../..");
const root=await mkdtemp(join(tmpdir(),"aurum-native-mission-")),project=join(root,"relay-yard");
await cp(join(repository,"examples/relay-yard"),project,{recursive:true,filter:path=>![".godot",".aurum","dist",".git"].includes(basename(path))});
const environment={...process.env,AURUM_GODOT:resolve(godot),AURUM_STUDIO_HOME:join(root,"state"),APPDATA:join(root,"user"),LOCALAPPDATA:join(root,"user")};
const server=spawn(resolve(binary),["studio",project,"--json","--no-open","--port","0"],{env:environment,windowsHide:true});
const exited=new Promise((resolve,reject)=>{server.once("error",reject);server.once("exit",resolve);});
const bounded=(promise,ms,label)=>new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error(`${label} deadline exceeded`)),ms);promise.then(value=>{clearTimeout(timer);resolve(value);},error=>{clearTimeout(timer);reject(error);});});
let output="",address;server.stderr.on("data",()=>{});
let checks=0;const check=(value,message)=>{assert.ok(value,message);checks++;};
try {
    address=await bounded(new Promise((resolve,reject)=>{server.once("error",reject);server.stdout.on("data",data=>{output+=data;const line=output.split("\n").find(line=>line.startsWith("{"));if(line){try{resolve(JSON.parse(line));}catch{}}});}),15000,"Studio startup");
    const url=new URL(address.url),token=url.searchParams.get("t");
    const request=async input=>{
        const response=await fetch(`${url.origin}/api/project`,{method:"POST",headers:{"Content-Type":"application/json","X-Aurum-Token":token},body:JSON.stringify(input),signal:AbortSignal.timeout(180000)});
        const value=await response.json();if(!response.ok || value.ok===false)throw new Error(JSON.stringify(value));return value;
    };
    const input=async(action,pressed)=>request({op:"native_input",input_actions:[{action,pressed}]});
    const state=async()=>(await request({op:"native_checkpoint"})).checkpoint.custom;
    const wait=async(predicate,label)=>{const deadline=Date.now()+15000;while(Date.now()<deadline){const value=await state();if(predicate(value))return value;await new Promise(resolve=>setTimeout(resolve,50));}throw new Error(`${label} was not reached`);};
    const started=await request({op:"native_start",rendered:false});check(started.running,"native real mission starts");
    await input("start",true);await wait(state=>state.phase==="play","normal mission start");await input("start",false);
    const beforeMove=await state();
    await input("move_left",true);await input("dash",true);
    await wait(state=>state.player.dash_count>beforeMove.player.dash_count && state.position[0]<beforeMove.position[0]-0.5,"normal movement and dash");
    await input("move_left",false);await input("dash",false);
    await input("pause",true);await wait(state=>state.paused,"mission pause");await input("pause",false);
    const before=await state();
    check(before.enemies.length>=3 && before.player.dash_count>0,"real enemies and durable action progress exist");
    const path=join(project,"godot/yard.gd"),source=await readFile(path,"utf8");
    await writeFile(path,`${source}\n# Compatible native mission revision.\n`);
    let replaced;
    try {replaced=await request({op:"native_replace"});}
    catch(error){await writeFile(join(root,"failure.json"),JSON.stringify({error:error.message,before,status:await request({op:"native_status"}),after:await state()},null,2));throw error;}
    check(replaced.state_preserved && replaced.pid!==started.pid,"native real mission replaced after verified restore");
    const after=await state();
    for(const key of ["phase","paused","powered","carried","elapsed","kills","interactions","rng_state","player","enemies","projectiles","camera_state","view"]){assert.deepEqual(after[key],before[key],`mission ${key} preserved`);checks++;}
    await writeFile(join(root,"verification.json"),JSON.stringify({ok:true,checks,started,replaced,before,after,scope:"real headless native mission, not rendered quality/device qualification"},null,2));
    await request({op:"native_stop"});
} finally {
    if(address){const url=new URL(address.url);try{await fetch(`${url.origin}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":url.searchParams.get("t")},signal:AbortSignal.timeout(5000)});}catch{}}
    try{await bounded(exited,10000,"Studio shutdown");}catch{server.kill();await bounded(exited,5000,"Studio termination");}
}
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
