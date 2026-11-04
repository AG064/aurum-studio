// Shared setup for genuine asset interface/render gates. Does not run by import.
import assert from "node:assert/strict";
import {mkdir,readFile,writeFile,opendir,lstat,open} from "node:fs/promises";
import {createHash} from "node:crypto";
import {join} from "node:path";
export const hash=bytes=>createHash("sha256").update(bytes).digest("hex");
export const assets=[
    ["assets/models_yard/antenna.glb","4A1B37D8B6086B4037CE0A5D528EAE29556894872A8B2CB12C4D1F2A7543C147"],
    ["assets/textures_yard/paint_wear.png","A92A7276ADA864FADCFFBB017B3D305C2B67A6EF77E7E7F795C1354C41827299"],
    ["assets/textures_yard/plate_seams.png","BE637BF95613E19B2DBABA55E089B3CB54165F152DA69EBE2A7DE9DC11211C67"]
];
export const clip={op:"animate_transform",node:"Animator",animation:"sweep",length:2,loop:true,tracks:[{target:"Antenna",property:"rotation",keys:[{time:0,value:[0,0,0,1]},{time:1,value:[0,Math.SQRT1_2,0,Math.SQRT1_2]},{time:2,value:[0,0,0,1]}]}]};
export const initialOperations=[{op:"instance",source:"res://assets/models_yard/antenna.glb",name:"Antenna"},{op:"create",type:"AnimationPlayer",name:"Animator"}];
export const find=(tree,path)=>{if(tree.path===path)return tree;for(const child of tree.children){const found=find(child,path);if(found)return found;}return null;};
export const materialEdit=material=>({op:"set",node:"Antenna/antenna_mesh",properties:{"surface_material_override/2":{resource:"StandardMaterial3D",properties:{albedo_color:{r:material.albedo_color[0],g:material.albedo_color[1],b:material.albedo_color[2],a:material.albedo_color[3]},metallic:material.metallic,roughness:material.roughness,albedo_texture:{resource_path:"res://assets/textures_yard/paint_wear.png",expected_type:"Texture2D"},uv1_triplanar:true,uv1_scale:{x:0.35,y:0.35,z:0.35}}}}});
export async function prepareProject(repository,project){
    await mkdir(project);for(const [relative,expected]of assets){const bytes=await readFile(join(repository,"examples/relay-yard/godot",relative));assert.equal(hash(bytes).toUpperCase(),expected);const path=join(project,relative);await mkdir(join(path,".."),{recursive:true});await writeFile(path,bytes);}
    await writeFile(join(project,"aurum.toml"),'schema_version=1\nname="asset-interface"\ngodot_version="4.7"\nmodules=[]\n');
    await writeFile(join(project,"project.godot"),'config_version=5\n[application]\nconfig/name="Asset authoring fixture"\n[rendering]\nrenderer/rendering_method="gl_compatibility"\n[display]\nwindow/size/viewport_width=960\nwindow/size/viewport_height=720\n');
    await mkdir(join(project,".godot"));await writeFile(join(project,".godot/cache-sentinel"),"original working import cache\n");
}
export async function inventory(directory,{working=false}={}){
    const rows=[];let entries=0,total=0n;
    const stable=(a,b)=>{assert.ok(b.isFile()&&!b.isSymbolicLink());for(const key of ["dev","ino","size","birthtimeNs","ctimeNs","mtimeNs"])assert.equal(a[key],b[key]);};
    async function digest(path,metadata){if(metadata.size===0n){stable(metadata,await lstat(path,{bigint:true}));return hash(Buffer.alloc(0));}const file=await open(path,"r"),digest=createHash("sha256"),buffer=Buffer.alloc(65536);let offset=0;try{stable(metadata,await file.stat({bigint:true}));while(offset<=Number(metadata.size)){const {bytesRead}=await file.read(buffer,0,Math.min(buffer.length,Number(metadata.size)-offset+1),offset);if(!bytesRead)break;offset+=bytesRead;assert.ok(offset<=Number(metadata.size));digest.update(buffer.subarray(0,bytesRead));}assert.equal(offset,Number(metadata.size));stable(metadata,await file.stat({bigint:true}));stable(metadata,await lstat(path,{bigint:true}));return digest.digest("hex");}finally{await file.close();}}
    async function walk(path,prefix="",depth=0){assert.ok(depth<=24);const names=[];for await(const entry of await opendir(path)){assert.ok(++entries<=4096);names.push(entry.name);}for(const name of names.sort()){if(working&&!prefix&&[".aurum","dist"].includes(name))continue;const full=join(path,name),relative=prefix+name,meta=await lstat(full,{bigint:true});assert.equal(meta.isSymbolicLink(),false);if(meta.isDirectory()){rows.push([relative+"/","directory"]);await walk(full,relative+"/",depth+1);}else{assert.ok(meta.size<=32n*1024n*1024n);total+=meta.size;assert.ok(total<=64n*1024n*1024n);rows.push([relative,await digest(full,meta)]);}}}
    await walk(directory);return rows.sort(([a],[b])=>a<b?-1:a>b?1:0);
}
export const observer=`extends Node3D
var samples: Array = []
var proof_directory := ""
var report_path := ""
var capture_callback
func _ready():
\t$Camera.look_at(Vector3(0,2.4,0))
\t$Animator.play("sweep")
\tvar args = OS.get_cmdline_user_args()
\tvar proof = args.find("--asset-proof-output")
\tif proof >= 0 and proof+1 < args.size(): proof_directory = args[proof+1]
\tvar report = args.find("--aurum-report")
\tif report >= 0 and report+1 < args.size(): report_path = args[report+1]
\tif OS.has_feature("web"):
\t\tcapture_callback = JavaScriptBridge.create_callback(_capture_pause)
\t\tJavaScriptBridge.get_interface("window").aurumAssetPause = capture_callback
func _capture_pause(args):
\tif args.size() == 1 and args[0] is bool:
\t\tget_tree().paused = args[0]
\t\tJavaScriptBridge.eval("window.aurumAssetSample=" + JSON.stringify(current_sample()))
func current_sample() -> Dictionary:
\tvar texture = $Antenna/antenna_mesh.get_active_material(2).albedo_texture
\treturn {"clip":$Animator.current_animation,"playing":$Animator.is_playing(),"paused":get_tree().paused,"time":$Animator.current_animation_position,"rotation":[$Antenna.quaternion.x,$Antenna.quaternion.y,$Antenna.quaternion.z,$Antenna.quaternion.w],"texture":texture.resource_path,"texture_size":[texture.get_width(),texture.get_height()],"sample_count":samples.size()}
func _process(_delta):
\tvar sample = current_sample()
\tif samples.size() < 2 and sample.time >= (0.35 if samples.is_empty() else 0.95):
\t\tsamples.append(sample)
\t\tif not proof_directory.is_empty():
\t\t\tawait RenderingServer.frame_post_draw
\t\t\tget_viewport().get_texture().get_image().save_png(proof_directory.path_join("native-pose-%d.png" % samples.size()))
\t\tif samples.size() == 2 and not report_path.is_empty():
\t\t\tvar report = FileAccess.open(report_path,FileAccess.WRITE)
\t\t\treport.store_string(JSON.stringify({"ok":true,"samples":samples,"source":"AnimationPlayer normal playback; observer never sets transforms"}))
\t\t\treport.close()
\tif OS.has_feature("web"):
\t\tJavaScriptBridge.eval("window.aurumAssetSample=" + JSON.stringify(sample))
func aurum_capture_state():
\treturn {"schema":1,"sample":current_sample(),"samples":samples}
func aurum_restore_state(_value):
\treturn false
`;
export const presentationOperations=[{op:"create",type:"Camera3D",name:"Camera",properties:{position:{x:7,y:5,z:9},current:true}},{op:"create",type:"DirectionalLight3D",name:"Light",properties:{rotation:{x:-0.6,y:-0.4,z:0},light_energy:1.8}},{op:"attach_script",node:".",script:"res://asset_observer.gd"}];
