// Native scene inspection can perform a cold, bounded resource import.
// Separate that operation from the ordinary UI-ready assertion in each test.
export async function openWorkspace(page, endpoint) {
    const [inspection] = await Promise.all([
        page.waitForResponse(response => {
            if (new URL(response.url()).pathname !== "/api/project") return false;
            try { return response.request().postDataJSON()?.op === "scene_inspect"; }
            catch { return false; }
        }, { timeout: 180000 }),
        page.goto(endpoint),
    ]);
    const result = await inspection.json();
    if (!inspection.ok() || result.ok !== true) {
        throw new Error(`Workspace scene inspection failed: ${result.error || inspection.status()}`);
    }
}
