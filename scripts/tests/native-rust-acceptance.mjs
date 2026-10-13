// Real Rust compilation and state-verified native handoff, with no source import.
import assert from "node:assert/strict";
import {spawn,spawnSync} from "node:child_process";
import {mkdtemp,mkdir,writeFile,readFile,readdir} from "node:fs/promises";
import {tmpdir} from "node:os";
import {join,resolve,dirname} from "node:path";
import {fileURLToPath} from "node:url";
const [binary,godot]=process.argv.slice(2);assert.ok(binary&&godot,"Usage: native-rust-acceptance.mjs <aurum> <godot>");
const repo=resolve(dirname(fileURLToPath(import.meta.url)),"../..");
const locked=await readFile(join(repo,"Cargo.lock"),"utf8");
const revision=locked.match(/name = "godot"[\s\S]*?source = "git\+https:\/\/github.com\/godot-rust\/gdext[^#]*#([a-f0-9]{40})"/)[1];
const root=await mkdtemp(join(tmpdir(),"aurum-native-rust-")),project=join(root,"project");
await mkdir(join(project,"src"),{recursive:true});await mkdir(join(project,"addons/native/bin"),{recursive:true});
await writeFile(join(project,"aurum.toml"),'schema_version=1\nname="native-rust-fixture"\nrust_package="aurum-native-fixture"\naddon_destination="addons/native"\nmodules=[]\n');
await writeFile(join(project,"Cargo.toml"),`[package]\nname="aurum-native-fixture"\nversion="0.1.0"\nedition="2021"\n[lib]\nname="native_game_logic"\ncrate-type=["cdylib"]\n[dependencies]\ngodot={git="https://github.com/godot-rust/gdext",rev="${revision}",default-features=false,features=["api-4-7"]}\n`);
const rust='use godot::prelude::*;\nuse godot::classes::Node;\n#[derive(GodotClass)]\n#[class(base=Node)]\nstruct RustState { #[export] counter:i64, #[export] cooldown:f64, #[export] mission_step:i64, #[export] build_revision:i64, base:Base<Node> }\n#[godot_api]\nimpl INode for RustState { fn init(base:Base<Node>)->Self { Self {counter:41,cooldown:3.5,mission_step:3,build_revision:1,base} } }\n#[godot_api]\nimpl RustState { #[func] fn boost(&mut self) { self.counter += self.build_revision; } }\nstruct NativeGame;\n#[gdextension]\nunsafe impl ExtensionLibrary for NativeGame {}\n';
await writeFile(join(project,"src/lib.rs"),rust);
await writeFile(join(project,"addons/native/bin/native.gdextension"),'[configuration]\nentry_symbol="gdext_rust_init"\ncompatibility_minimum="4.7"\n[libraries]\nwindows.debug.x86_64="./active.dll"\n');
const staleDll="not a working native library";
await writeFile(join(project,"addons/native/bin/active.dll"),staleDll);
await writeFile(join(project,"project.godot"),'config_version=5\n[application]\nrun/main_scene="res://main.tscn"\n[input]\nmove_right={"deadzone":0.2,"events":[]}\ndash={"deadzone":0.2,"events":[]}\npause={"deadzone":0.2,"events":[]}\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n');
const script='extends Node3D\nvar inventory=["cell","cargo"]\nvar enemies=["guard-4","guard-9"]\nfunc _ready():\n\tprocess_mode=Node.PROCESS_MODE_ALWAYS\nfunc _physics_process(delta):\n\tif get_tree().paused: return\n\tif Input.is_action_pressed("move_right"): position.x+=5.0*delta\nfunc _input(event):\n\tif event.is_action_pressed("pause"): get_tree().paused=not get_tree().paused\n\tif event.is_action_pressed("dash") and not get_tree().paused: $RustState.boost()\nfunc aurum_capture_state():\n\treturn {"schema":1,"position":position,"inventory":inventory,"enemies":enemies,"counter":$RustState.counter,"cooldown":$RustState.cooldown,"mission_step":$RustState.mission_step}\nfunc aurum_restore_state(state):\n\tif not state is Dictionary or state.get("schema")!=1 or not state.get("position") is Vector3: return false\n\tposition=state.position\n\tinventory=state.inventory.duplicate()\n\tenemies=state.enemies.duplicate()\n\t$RustState.counter=int(state.counter)\n\t$RustState.cooldown=float(state.cooldown)\n\t$RustState.mission_step=int(state.mission_step)\n\treturn true\n';
await writeFile(join(project,"main.gd"),script);
await writeFile(join(project,"main.tscn"),'[gd_scene load_steps=2 format=3]\n[ext_resource type="Script" path="res://main.gd" id="1"]\n[node name="Mission" type="Node3D"]\nscript=ExtResource("1")\n[node name="RustState" type="RustState" parent="."]\n');
const environment={...process.env,AURUM_GODOT:resolve(godot),AURUM_STUDIO_HOME:join(root,"state"),APPDATA:join(root,"user"),LOCALAPPDATA:join(root,"user")};
const generated=spawnSync("cargo",["generate-lockfile","--offline"],{cwd:project,env:environment,windowsHide:true,encoding:"utf8",timeout:60000});assert.ifError(generated.error);assert.equal(generated.status,0,generated.stderr);
const lock=await readFile(join(project,"Cargo.lock"),"utf8");
const server=spawn(resolve(binary),["studio",project,"--no-open","--json","--port","0"],{env:environment,windowsHide:true});
const done=new Promise((resolve,reject)=>{server.once("error",reject);server.once("exit",resolve);});server.stderr.on("data",()=>{});
const bound=(promise,ms,label)=>new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error(`${label} deadline exceeded`)),ms);promise.then(value=>{clearTimeout(timer);resolve(value);},error=>{clearTimeout(timer);reject(error);});});
let checks=0,output="",address;const check=(value,message)=>{assert.ok(value,message);checks++;};
try {
    address=await bound(new Promise((resolve,reject)=>{server.once("error",reject);server.stdout.on("data",data=>{output+=data;const line=output.split("\n").find(line=>line.startsWith("{"));if(line){try{resolve(JSON.parse(line));}catch{}}});}),15000,"Studio startup");
    const url=new URL(address.url),token=url.searchParams.get("t");
    const request=async input=>{const response=await fetch(`${url.origin}/api/project`,{method:"POST",headers:{"Content-Type":"application/json","X-Aurum-Token":token},body:JSON.stringify(input),signal:AbortSignal.timeout(330000)});return {status:response.status,value:await response.json()};};
    const action=async input=>{const result=await request(input);assert.equal(result.status,200,JSON.stringify(result.value));return result.value;};
    const started=await action({op:"native_start",rendered:false});check(started.running && started.native_dependencies.compiled.sha256.length===64,"actual private Rust build boots and reports artifact identity");
    check(started.native_dependencies.compiled.target==="native_game_logic","custom library target names come from Cargo metadata");
    const input=async(actionName,pressed)=>action({op:"native_input",input_actions:[{action:actionName,pressed}]});
    await input("dash",true);await input("dash",false);await input("move_right",true);
    const deadline=Date.now()+15000;let before;
    while(Date.now()<deadline){before=await action({op:"native_checkpoint"});if(before.checkpoint.custom.position.value[0]>0.5)break;await new Promise(resolve=>setTimeout(resolve,50));}
    await input("move_right",false);await input("pause",true);await input("pause",false);
    before=await action({op:"native_checkpoint"});check(before.checkpoint.paused && before.checkpoint.custom.counter===42,"normal input uses the real Rust method and pauses the mission");
    check(!(await readdir(project)).includes(".godot") && (await readFile(join(project,"addons/native/bin/active.dll"),"utf8"))===staleDll,"working import directory and stale DLL remain untouched");
    const changed=rust.replace("build_revision:1","build_revision:2");await writeFile(join(project,"src/lib.rs"),changed);
    const replaced=await action({op:"native_replace"});check(replaced.state_preserved && replaced.pid!==started.pid,"actual Rust implementation edit replaces native run");
    let after=await action({op:"native_checkpoint"});assert.deepEqual(after.checkpoint.custom,before.checkpoint.custom);checks++;
    const inspected=await action({op:"native_inspect",node_path:"RustState"});check(inspected.properties.find(property=>property.name==="build_revision").value===2,"new native constructor executes after verified state restore");
    await input("pause",true);await input("pause",false);await input("dash",true);await input("dash",false);await input("pause",true);await input("pause",false);
    after=await action({op:"native_checkpoint"});check(after.checkpoint.custom.counter===44,"new native method implementation runs after replacement");
    await writeFile(join(project,"src/lib.rs"),changed+'\ninvalid Rust syntax!\n');
    check((await request({op:"native_replace"})).status===400,"compiler failure refuses replacement");
    check((await action({op:"native_status"})).pid===replaced.pid,"compiler failure keeps the accepted native process");
    assert.deepEqual((await action({op:"native_checkpoint"})).checkpoint.custom,after.checkpoint.custom);checks++;
    await writeFile(join(project,"src/lib.rs"),changed);
    const manifest=join(project,"addons/native/bin/native.gdextension"),originalManifest=await readFile(manifest,"utf8");
    await writeFile(manifest,originalManifest.replace('"./active.dll"','"C:/outside.dll"'));
    check((await request({op:"native_replace"})).status===400,"external library paths are refused before loading");
    check((await action({op:"native_status"})).pid===replaced.pid,"dependency refusal preserves accepted game");
    await writeFile(manifest,originalManifest);
    check((await readFile(join(project,"Cargo.lock"),"utf8"))===lock && (await readFile(join(project,"main.gd"),"utf8"))===script,"private compilation preserves working lockfile and game script");
    await action({op:"native_stop"});
    await writeFile(join(root,"verification.json"),JSON.stringify({ok:true,checks,started,replaced,before:before.checkpoint,after:after.checkpoint,scope:"actual self-contained offline Rust native build/handoff; not graphics or device qualification"},null,2));
} finally {
    if(address){const url=new URL(address.url);try{await fetch(`${url.origin}/api/stop`,{method:"POST",headers:{"X-Aurum-Token":url.searchParams.get("t")},signal:AbortSignal.timeout(5000)});}catch{}}
    try{await bound(done,10000,"Studio shutdown");}catch{server.kill();await bound(done,5000,"Studio termination");}
}
console.log(JSON.stringify({checks,status:"passed",evidence:root}));
