// Real changed headless worker proof. Compiled shared routing and rendered exports are later gates.
import assert from "node:assert/strict";
import {spawn,spawnSync} from "node:child_process";
import {mkdtemp,mkdir,cp,readFile,writeFile,lstat,opendir,realpath,rename} from "node:fs/promises";
import {createHash} from "node:crypto";
import {tmpdir} from "node:os";
import {join,resolve} from "node:path";
const [godot,source]=process.argv.slice(2);
assert.ok(godot&&source,"Usage: asset-authoring-acceptance.mjs <pinned Godot> <frozen changed source>");
for(const path of [godot,source,tmpdir()])assert.match(resolve(path),/^A:[\\/]/i);
const runtime=await realpath(resolve(godot)),repository=await realpath(resolve(source)),root=await mkdtemp(join(tmpdir(),"aurum-asset-authoring-"));
const working=join(root,"working"),privateProject=join(root,"private"),operations=join(root,"operations"),profile=join(root,"profile"),temporary=join(root,"tmp");
for(const directory of [working,privateProject,operations,profile,temporary])await mkdir(directory);
await mkdir(join(profile,"AppData/Roaming"),{recursive:true});
const environment={...process.env,TEMP:temporary,TMP:temporary,USERPROFILE:profile,APPDATA:profile,LOCALAPPDATA:profile,XDG_DATA_HOME:profile,XDG_CONFIG_HOME:profile,XDG_CACHE_HOME:profile,AURUM_HOME:join(root,"state"),AURUM_STUDIO_HOME:join(root,"state")};
const sha=bytes=>createHash("sha256").update(bytes).digest("hex");
const save=async(path,bytes)=>{await mkdir(join(path,".."),{recursive:true});await writeFile(path,bytes);};
const runtimeSha=sha(await readFile(runtime));assert.equal(runtimeSha.toUpperCase(),"B2CA888D5115A6CEDEE564764A2EE494A625F2EC2EDBABD010FE33C9A88A6BF8");
const helper=join(repository,"crates/aurum-studio-core/src/headless.gd"),helperSha=sha(await readFile(helper));
const assetRows=[
    ["assets/models_yard/antenna.glb","4A1B37D8B6086B4037CE0A5D528EAE29556894872A8B2CB12C4D1F2A7543C147"],
    ["assets/textures_yard/paint_wear.png","A92A7276ADA864FADCFFBB017B3D305C2B67A6EF77E7E7F795C1354C41827299"],
    ["assets/textures_yard/plate_seams.png","BE637BF95613E19B2DBABA55E089B3CB54165F152DA69EBE2A7DE9DC11211C67"]
];
for(const [path,expected]of assetRows){const bytes=await readFile(join(repository,"examples/relay-yard/godot",path));assert.equal(sha(bytes).toUpperCase(),expected);await save(join(working,path),bytes);await save(join(privateProject,path),bytes);}
await save(join(working,"project.godot"),'config_version=5\n[application]\nconfig/name="Typed asset fixture"\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n');
await cp(join(working,"project.godot"),join(privateProject,"project.godot"));
await save(join(working,".godot/cache-sentinel"),"original working cache\n");
const evidence={ok:false,phase:"setup",root,working,private_project:privateProject,runtime_sha256:runtimeSha,helper_sha256:helperSha,assets:assetRows,events:[],scope:"Real Godot changed worker author/reopen and refusal proof; compiled shared Rust/CLI/HTTP/MCP schema execution and rendered native/Web proof pending"};let checks=0,id=0;
const check=(value,label)=>{assert.ok(value,label);checks++;};const equal=(a,b,label)=>{assert.deepEqual(a,b,label);checks++;};
const persist=async()=>{const text=JSON.stringify({...evidence,checks},null,2);assert.ok(Buffer.byteLength(text)<=65536);await writeFile(join(root,"verification.json"),text);};
const inventory=async directory=>{const rows=[];let seen=0,total=0;async function walk(path,prefix="",depth=0){assert.ok(depth<=24);const names=[];for await(const entry of await opendir(path)){assert.ok(++seen<=4096);names.push(entry.name);}for(const name of names.sort()){const full=join(path,name),relative=prefix+name,meta=await lstat(full);assert.equal(meta.isSymbolicLink(),false);if(meta.isDirectory()){rows.push([relative+"/","directory"]);await walk(full,relative+"/",depth+1);}else{assert.ok(meta.isFile()&&meta.size<=16*1024*1024);total+=meta.size;assert.ok(total<=64*1024*1024);const bytes=await readFile(full);const after=await lstat(full);assert.equal(bytes.length,meta.size);assert.equal(after.size,meta.size);assert.equal(after.mtimeMs,meta.mtimeMs);rows.push([relative,sha(bytes)]);}}}await walk(directory);return rows.sort(([a],[b])=>a<b?-1:a>b?1:0);};
const run=async(args,label,timeout=60000)=>{
    evidence.phase=label;await persist();const child=spawn(runtime,args,{env:environment,windowsHide:true,stdio:["ignore","pipe","pipe"]});let stdout="",stderr="",dropped=0,timedOut=false;
    const append=(text,chunk)=>{const value=String(chunk),available=Math.max(0,65536-Buffer.byteLength(text));dropped+=Math.max(0,Buffer.byteLength(value)-available);return text+value.slice(0,available);};
    child.stdout.on("data",data=>{stdout=append(stdout,data);});child.stderr.on("data",data=>{stderr=append(stderr,data);});
    const result=await new Promise((resolve,reject)=>{child.once("error",reject);const timer=setTimeout(()=>{timedOut=true;child.kill();},timeout);child.once("close",(code,signal)=>{clearTimeout(timer);resolve({code,signal});});});
    const event={label,pid:child.pid,exit_code:result.code,signal:result.signal,timed_out:timedOut,owned_process_closed:true,dropped_bytes:dropped};evidence.events.push(event);
    await writeFile(join(operations,`${++id}-${label.replace(/[^a-z0-9-]/gi,"_")}.log`),JSON.stringify({event,stdout,stderr},null,2));await persist();assert.equal(timedOut,false,`${label} bounded owned execution`);return {...event,stdout,stderr};
};
const request=async(input,label,expectSuccess=true)=>{
    const folder=join(operations,`request-${id+1}`);await mkdir(folder);const inputPath=join(folder,"input.json"),outputPath=join(folder,"output.json"),staged=join(folder,"stage.tscn");
    await writeFile(inputPath,JSON.stringify({...input,output_scene:staged,output_project:join(folder,"project.godot")}));
    const result=await run(["--headless","--path",privateProject,"--script",helper,"--",inputPath,outputPath],label);
    const value=JSON.parse(await readFile(outputPath,"utf8"));check(Buffer.byteLength(JSON.stringify(value))<=512*1024,"Bounded actual headless inspection result");evidence.last={label,value};await persist();
    if(expectSuccess){check(result.exit_code===0&&value.ok===true&&!/\b(?:SCRIPT ERROR|ERROR):/.test(result.stdout+result.stderr),`${label} real worker success without script errors`);}else{check(result.exit_code!==0&&value.ok===false,`${label} actual refusal`);assert.equal(await lstat(staged).catch(error=>{if(error.code==="ENOENT")return null;throw error;}),null,"Refusal produces no staged scene");checks++;}
    return {value,staged};
};
const find=(tree,path)=>{if(tree.path===path)return tree;for(const child of tree.children){const found=find(child,path);if(found)return found;}return null;};
const original=await inventory(working);
const preserved=async(expected,label)=>{equal(await inventory(working),expected,`${label}: entire working source/asset/cache inventory exact`);equal(sha(await readFile(runtime)),runtimeSha,`${label}: shared Godot unchanged`);};
const q=Math.SQRT1_2;
const clip={op:"animate_transform",node:"Animator",animation:"sweep",length:2,loop:true,tracks:[{target:"Antenna",property:"rotation",keys:[{time:0,value:[0,0,0,1]},{time:1,value:[0,q,0,q]},{time:2,value:[0,0,0,1]}]}]};
try {
    const contract=await readFile(join(repository,"crates/aurum-studio-core/src/project_contract.rs"),"utf8");check(contract.includes('"animate_transform"')&&contract.includes('let resource_reference')&&contract.includes('"additionalProperties":false'),"Formal shared schema source declares bounded animation and closed typed references");evidence.compiled_schema_execution="unrun, root Cargo/build gate required";
    const imported=await run(["--headless","--editor","--path",privateProject,"--import"],"private-import",120000);check(imported.exit_code===0&&!/\b(?:SCRIPT ERROR|ERROR):/.test(imported.stdout+imported.stderr),"Real licensed GLB/PNG private import");await preserved(original,"private import");
    const created=await request({op:"scene_create",scene:"res://authored.tscn",name:"AssetDisplay",root_type:"Node3D",operations:[{op:"instance",source:"res://assets/models_yard/antenna.glb",name:"Antenna"},{op:"create",type:"AnimationPlayer",name:"Animator"} ]},"create-imported-model");
    check(find(created.value.tree,"Antenna/antenna_mesh")?.mesh.surface_count===5,"Real imported antenna has five inspectable mesh surfaces");
    const baseScene=await readFile(created.staged);await rename(created.staged,join(working,"authored.tscn"));await writeFile(join(privateProject,"authored.tscn"),baseScene);
    const baseInventory=(await inventory(working));const baseline=await request({op:"scene_inspect",scene:"res://authored.tscn"},"reopen-original");const originalSurfaces=find(baseline.value.tree,"Antenna/antenna_mesh").mesh.surfaces;
    const meshMaterial=originalSurfaces[2].material;
    const textureMaterial={resource:"StandardMaterial3D",properties:{albedo_color:{r:meshMaterial.albedo_color[0],g:meshMaterial.albedo_color[1],b:meshMaterial.albedo_color[2],a:meshMaterial.albedo_color[3]},metallic:meshMaterial.metallic,roughness:meshMaterial.roughness,albedo_texture:{resource_path:"res://assets/textures_yard/paint_wear.png",expected_type:"Texture2D"},uv1_triplanar:true,uv1_scale:{x:0.35,y:0.35,z:0.35}}};
    const authored=await request({op:"scene_edit",scene:"res://authored.tscn",operations:[{op:"set",node:"Antenna/antenna_mesh",properties:{"surface_material_override/2":textureMaterial}},clip]},"bind-texture-and-author-clip");
    const authoredBytes=await readFile(authored.staged);await rename(authored.staged,join(working,"authored.tscn"));await writeFile(join(privateProject,"authored.tscn"),authoredBytes);
    const expected=baseInventory.map(([path,hash])=>[path,path==="authored.tscn"?sha(authoredBytes):hash]);await preserved(expected,"author commit");
    const inspected=await request({op:"scene_inspect",scene:"res://authored.tscn"},"real-reopen-authored");const mesh=find(inspected.value.tree,"Antenna/antenna_mesh"),animation=find(inspected.value.tree,"Animator").animation_libraries[0].animations[0];
    check(mesh.mesh.surfaces[2].material.albedo_texture.path==="res://assets/textures_yard/paint_wear.png"&&mesh.mesh.surfaces[2].material.albedo_texture.width>0&&mesh.mesh.surfaces[2].material.albedo_texture.height>0,"Imported child texture-backed material override persists after real reopen");
    for(const index of [0,1,3,4])equal(mesh.mesh.surfaces[index].material,originalSurfaces[index].material,"Untouched imported material palette remains exact");
    check(animation.name==="sweep"&&animation.length===2&&animation.loop_mode===1&&animation.tracks.length===1&&animation.tracks[0].type===2&&animation.tracks[0].target==="Antenna"&&animation.tracks[0].key_count===3,"Saved local rotation animation and explicit key count persist");equal(animation.tracks[0].keys.map(key=>key.time),[0,1,2],"Animation key times exact after reopen");check(Math.abs(animation.tracks[0].keys[1].value[1]-q)<1e-6&&Math.abs(animation.tracks[0].keys[1].value[3]-q)<1e-6,"Normalized quaternion key values persist after reopen");check(inspected.value.truncated===false&&inspected.value.inspection_limits.resources===128,"Material and animation inspection has explicit bounded result metadata");evidence.reopened={scene_sha256:sha(authoredBytes),mesh:mesh.mesh,animations:find(inspected.value.tree,"Animator").animation_libraries};
    const badRef=(path,type="Texture2D")=>({op:"set",node:"Antenna/antenna_mesh",properties:{material_override:{resource:"StandardMaterial3D",properties:{albedo_texture:{resource_path:path,expected_type:type}}}}});
    const refusals=[
        ["missing-texture",badRef("res://assets/no.png")],
        ["wrong-resource-type",badRef("res://assets/models_yard/antenna.glb")],
        ["escape-resource",badRef("res://../outside.png")],
        ["unknown-resource-class",badRef("res://assets/textures_yard/paint_wear.png","Script")],
        ["method-track",{...clip,animation:"bad-method",tracks:[{target:"Antenna",property:"method",keys:[{time:0,value:[0,0,0]}]}]}],
        ["escape-animation-target",{...clip,animation:"bad-target",tracks:[{...clip.tracks[0],target:"../Outside"}]}],
        ["nonascending-key-times",{...clip,animation:"bad-times",tracks:[{...clip.tracks[0],keys:[{time:1,value:[0,0,0,1]},{time:0,value:[0,0,0,1]}]}]}],
        ["unnormalized-quaternion",{...clip,animation:"bad-rotation",tracks:[{...clip.tracks[0],keys:[{time:0,value:[0,0,0,2]}]}]}],
        ["unknown-animation-field",{...clip,animation:"bad-field",method:"queue_free"}],
        ["unknown-material-property",{op:"set",node:"Antenna/antenna_mesh",properties:{material_override:{resource:"StandardMaterial3D",properties:{unknown_property:true}}}}]
    ];
    const outside=join(root,"outside");await mkdir(outside);await cp(join(working,"assets/textures_yard/paint_wear.png"),join(outside,"texture.png"));const link=join(privateProject,"linked");const junction=spawnSync("cmd.exe",["/c","mklink","/J",link.replaceAll("/","\\"),outside.replaceAll("/","\\")],{windowsHide:true,encoding:"utf8",timeout:10000});assert.ifError(junction.error);assert.equal(junction.status,0,junction.stderr);refusals.push(["junction-resource",badRef("res://linked/texture.png")]);
    for(const [label,operation]of refusals){const prior=sha(await readFile(join(working,"authored.tscn")));const rejected=await request({op:"scene_edit",scene:"res://authored.tscn",operations:[{op:"set",node:".",properties:{position:{x:999,y:0,z:0}}},operation]},label,false);equal(sha(await readFile(join(working,"authored.tscn"))),prior,`${label}: scene hash exact after mixed batch refusal`);await preserved(expected,label);evidence.refusals??=[];evidence.refusals.push({label,error:rejected.value.error});}
    const final=await request({op:"scene_inspect",scene:"res://authored.tscn"},"final-reopen");equal(find(final.value.tree,"Animator").animation_libraries,find(inspected.value.tree,"Animator").animation_libraries,"Every authored clip/key remains unchanged after refusals");await preserved(expected,"final proof");equal(sha(await readFile(helper)),helperSha,"Frozen changed worker identity unchanged");evidence.ok=true;evidence.phase="complete";
}catch(error){evidence.error=error.stack??String(error);throw error;}finally{await persist();}
console.log(JSON.stringify({status:"passed",checks,evidence:root}));
