// Offline source processing. Downloads are deliberately separate from this tool.
import { NodeIO } from "@gltf-transform/core";
import { center, dedup, prune, resample, getBounds } from "@gltf-transform/functions";
import { ALL_EXTENSIONS } from "@gltf-transform/extensions";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { resolve, join } from "node:path";

const [sourceRoot, outputRoot] = process.argv.slice(2);
if (!sourceRoot || !outputRoot) throw new Error("Usage: prepare_assets.mjs <extracted-pack-root> <new-output-directory>");
await mkdir(outputRoot, { recursive: false });
const entries = {
    courier: ["space", "Models/GLTF format/craft_speederA.glb"],
    drone: ["space", "Models/GLTF format/craft_speederD.glb"],
    warden: ["space", "Models/GLTF format/craft_miner.glb"],
    generator: ["space", "Models/GLTF format/machine_generatorLarge.glb"],
    transmitter: ["space", "Models/GLTF format/machine_wireless.glb"],
    reactor: ["space", "Models/GLTF format/machine_barrelLarge.glb"],
    antenna: ["space", "Models/GLTF format/satelliteDish_detailed.glb"],
    cargo: ["space", "Models/GLTF format/craft_cargoA.glb"],
    hangar: ["space", "Models/GLTF format/hangar_smallA.glb"],
    barrel: ["space", "Models/GLTF format/barrels.glb"],
    pipe: ["space", "Models/GLTF format/pipe_straight.glb"],
    crate: ["station", "Models/GLB format/container-wide.glb"],
    crate_tall: ["station", "Models/GLB format/container-tall.glb"],
    terminal: ["station", "Models/GLB format/computer-wide.glb"],
    floor: ["station", "Models/GLB format/floor-detail.glb"],
    barrier: ["station", "Models/GLB format/structure-barrier.glb"],
};
const io = new NodeIO().registerExtensions(ALL_EXTENSIONS);
const manifest = { version: 1, format: "glTF 2.0", units: "source metres", assets: {} };
for (const [key, [pack, file]] of Object.entries(entries)) {
    const input = resolve(sourceRoot, pack, file);
    const document = await io.read(input);
    await document.transform(dedup(), prune(), resample(), center({ pivot: "below" }));
    const path = join(outputRoot, key + ".glb");
    await io.write(path, document);
    const json = (await io.writeJSON(document)).json;
    const data = await readFile(path);
    let vertices = 0;
    for (const mesh of document.getRoot().listMeshes()) {
        for (const primitive of mesh.listPrimitives()) vertices += primitive.getAttribute("POSITION")?.getCount() || 0;
    }
    const bounds = getBounds(document.getRoot().listScenes()[0]);
    const size = bounds.max.map((maximum, axis) => maximum - bounds.min[axis]);
    if (!size.every(value => Number.isFinite(value) && value > 0) || data.length > 1024 * 1024 || vertices > 20000) throw new Error(`Asset budget or bounds failed: ${key}`);
    manifest.assets[key] = { file: key + ".glb", source_pack: pack, source_file: file, sha256: createHash("sha256").update(data).digest("hex"), bytes: data.length, vertices, meshes: json.meshes?.length || 0, size, pivot: "ground-centre", animations: document.getRoot().listAnimations().map(animation => animation.getName()) };
    console.log(`${key}: ${data.length} bytes, ${vertices} vertices`);
}
await writeFile(join(outputRoot, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n", { flag: "wx" });
