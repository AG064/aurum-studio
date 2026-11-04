// Standalone exported Web evidence. Requires the genuine preceding delivery manifest.
import {test,expect} from "@playwright/test";
import {createServer} from "node:http";
import {readFile,realpath,lstat,writeFile} from "node:fs/promises";
import {resolve,join,extname,sep} from "node:path";
test("standalone authored texture and clip render at two normal playback poses",async({browser},info)=>{
    test.setTimeout(180000);const manifestPath=process.env.AURUM_ASSET_DELIVERY_MANIFEST;
    if(!manifestPath||!/^A:[\\/]/i.test(resolve(manifestPath)))throw new Error("A-only qualified asset delivery manifest is required");
    const manifest=JSON.parse(await readFile(manifestPath,"utf8")),webRoot=await realpath(manifest.web_directory);expect(webRoot).toMatch(/^A:[\\/]/i);
    const mime={".html":"text/html",".js":"application/javascript",".wasm":"application/wasm",".pck":"application/octet-stream",".png":"image/png"};let serverErrors=[];
    const server=createServer(async(request,response)=>{try{const pathname=decodeURIComponent(new URL(request.url,"http://localhost").pathname);const path=await realpath(resolve(webRoot,"."+pathname));if(!path.startsWith(webRoot+sep)||(await lstat(path)).isDirectory()){response.writeHead(403);response.end();return;}const bytes=await readFile(path);response.writeHead(200,{"Content-Type":mime[extname(path)]??"application/octet-stream","Cross-Origin-Opener-Policy":"same-origin","Cross-Origin-Embedder-Policy":"require-corp","Cache-Control":"no-store"});response.end(bytes);}catch(error){serverErrors.push(error.code??error.name);response.writeHead(404);response.end();}});
    await new Promise((resolve,reject)=>{server.once("error",reject);server.listen(0,"127.0.0.1",resolve);});const address=server.address(),url=`http://127.0.0.1:${address.port}/${manifest.web_html.split(/[\\/]/).at(-1)}`;
    const context=await browser.newContext({viewport:{width:960,height:720}}),page=await context.newPage(),errors=[],samples=[];page.on("pageerror",error=>errors.push(error.message));page.on("console",message=>{if(message.type()==="error"&&/SCRIPT ERROR|RuntimeError|TypeError/.test(message.text()))errors.push(message.text());});
    const sample=()=>page.evaluate(()=>window.aurumAssetSample??null);
    const capture=async(name)=>{await page.evaluate(()=>window.aurumAssetPause(true));const paused=await sample();expect(paused.paused).toBe(true);try{await page.screenshot({path:info.outputPath(name),timeout:30000});samples.push(paused);}finally{await page.evaluate(()=>window.aurumAssetPause(false));}return paused;};
    try{
        await page.goto(url);await expect(page.locator("canvas")).toBeVisible({timeout:120000});await expect.poll(async()=>Boolean(await sample()),{timeout:30000}).toBe(true);expect(await page.evaluate(()=>crossOriginIsolated)).toBe(true);
        await expect.poll(async()=>{const state=await sample();return state?.time>=0.25&&state.time<=0.5;},{timeout:15000}).toBe(true);const first=await capture("asset-web-pose-1.png");
        await expect.poll(async()=>{const state=await sample();return state?.time>=0.9&&state.time<=1.1;},{timeout:15000}).toBe(true);const second=await capture("asset-web-pose-2.png");
        for(const value of [first,second]){expect(value.clip).toBe("sweep");expect(value.playing).toBe(true);expect(value.texture).toBe("res://assets/textures_yard/paint_wear.png");expect(value.texture_size).toEqual([256,256]);}
        expect(Math.abs(first.rotation[1]-second.rotation[1])).toBeGreaterThan(0.1);expect(errors).toEqual([]);
        await writeFile(info.outputPath("asset-web-evidence.json"),JSON.stringify({scope:"Real standalone Web normal AnimationPlayer playback; paused only for capture; image inspection still required",manifest:manifestPath,url,samples,errors,serverErrors,visual_review:"pending"},null,2));
    }catch(error){try{await capture("asset-web-failure.png");}catch(captureError){await writeFile(info.outputPath("failure-capture.json"),JSON.stringify({error:captureError.message}));}throw error;}
    finally{await context.close();await new Promise(resolve=>server.close(resolve));}
});
