import { test, expect } from "@playwright/test";
import { mkdtemp, cp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, dirname, join, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { monitorHttp } from "./http-diagnostics.js";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
test("a real Rust GDExtension boots in an isolated Web preview", async ({ browser }, info) => {
    test.setTimeout(1200000);
    for (const key of ["AURUM_BINARY","AURUM_GODOT","AURUM_WEB_TEMPLATE"]) if (!process.env[key]) throw new Error(`Rust Web acceptance requires ${key}; it never silently skips`);
    if (!process.env.AURUM_EMSDK && !process.env.AURUM_RUST_TEST_STUDIO_HOME) throw new Error("Rust Web acceptance requires an SDK or an isolated Studio home with a managed SDK");
    const work = await mkdtemp(join(tmpdir(),"aurum-rust-browser-"));
    const project = process.env.AURUM_RUST_TEST_PROJECT || join(work,"project");
    if (!process.env.AURUM_RUST_TEST_PROJECT) await cp(repo,project,{recursive:true,filter:(path)=>!["target",".git",".godot",".aurum","node_modules","dist","logs"].includes(basename(path))});
    let output="", token="", origin="";
    const browserErrors=[];
    const child=spawn(resolve(process.env.AURUM_BINARY),["studio",project,"--no-open"],{windowsHide:true,env:{...process.env,AURUM_STUDIO_HOME:process.env.AURUM_RUST_TEST_STUDIO_HOME||join(work,"state"),APPDATA:join(work,"userdata"),XDG_DATA_HOME:join(work,"userdata")},stdio:["ignore","pipe","pipe"]});
    child.stdout.on("data",data=>output+=data);child.stderr.on("data",data=>output+=data);
    const context=await browser.newContext({viewport:{width:1488,height:1056}});
    const page=await context.newPage();
    const httpDiagnostics=monitorHttp(page);
    page.on("pageerror",error=>browserErrors.push(error.message));
    page.on("console",message=>{if(message.type()==="error")browserErrors.push(message.text());});
    try{
        await expect.poll(()=>output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/)?.[0],{timeout:30000}).toBeTruthy();
        const endpoint=output.match(/http:\/\/127\.0\.0\.1:\d+\/\?t=[a-zA-Z0-9_-]+/)[0];origin=new URL(endpoint).origin;token=new URL(endpoint).searchParams.get("t");
        const response=await context.request.post(origin+"/api/preview",{headers:{"X-Aurum-Token":token},data:{project},timeout:900000});
        const preview=await response.json();expect(preview.ok,preview.error).toBe(true);expect(preview.requires_cross_origin_isolation).toBe(true);
        await page.goto(preview.url);
        await expect(page.locator("#status")).toHaveCount(0,{timeout:120000});
        await expect(page.locator("canvas")).toBeVisible();
        expect(await page.evaluate(()=>crossOriginIsolated)).toBe(true);
        const inspected=await page.evaluate(async()=>{
            const deadline=Date.now()+30000;
            while(typeof window.aurumRuntimeRequest!=="function"&&Date.now()<deadline)await new Promise(resolve=>setTimeout(resolve,100));
            window.aurumRuntimeResponse="";
            window.aurumRuntimeRequest(JSON.stringify({op:"inspect"}));
            return JSON.parse(window.aurumRuntimeResponse);
        });
        expect(inspected.ok).toBe(true);
        const rustClass=await page.evaluate(()=>{
            window.aurumRuntimeResponse="";
            window.aurumRuntimeRequest(JSON.stringify({op:"class_info",class:"AurumNode"}));
            return JSON.parse(window.aurumRuntimeResponse);
        });
        expect(rustClass.registered).toBe(true);
        expect(rustClass.methods.length).toBeGreaterThan(0);
        expect(browserErrors).toEqual([]);
        await page.screenshot({path:info.outputPath("rust-web-game.png")});
        await writeFile(info.outputPath("evidence.json"),JSON.stringify({work,project,preview,inspected,rustClass},null,2));
    }finally{
        await context.close();
        if(origin)await fetch(origin+"/api/stop",{method:"POST",headers:{"X-Aurum-Token":token,"Content-Type":"application/json"},body:"{}"}).catch(()=>{});
        if(child.exitCode===null)await Promise.race([new Promise(resolve=>child.once("exit",resolve)),new Promise(resolve=>setTimeout(resolve,5000))]);
        if(child.exitCode===null)child.kill();
        await writeFile(info.outputPath("studio.log"),token?output.replaceAll(token,"[redacted]"):output);
        await writeFile(info.outputPath("browser-errors.json"),JSON.stringify(browserErrors,null,2));
        await writeFile(info.outputPath("http-diagnostics.json"),JSON.stringify(httpDiagnostics(),null,2));
    }
});
