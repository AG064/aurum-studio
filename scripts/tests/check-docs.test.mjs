import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { checkDocuments, headingAnchors, markdownLinks } from "../check-docs.mjs";

async function fixture(files) {
    const root = await mkdtemp(join(tmpdir(), "aurum-doc-check-"));
    for (const [path, text] of Object.entries(files)) {
        await mkdir(join(root, path, ".."), { recursive: true });
        await writeFile(join(root, path), text);
    }
    return root;
}

test("headings retain code words, Unicode, duplicates and explicit IDs", () => {
    assert.deepEqual([...headingAnchors('# Use `aurum`\n# Use aurum\n# Use aurum-1\n## Café\n<a id="custom"></a>\n')], ["custom", "use-aurum", "use-aurum-1", "use-aurum-1-1", "café"]);
});
test("fences are not documentation links or headings", () => {
    const text = '```md\n# Hidden\n[bad](missing.md)\n```\n~~~\n[bad](missing.png)\n~~~\n# Visible';
    assert.equal(markdownLinks(text).length, 0);
    assert.deepEqual([...headingAnchors(text)], ["visible"]);
});
test("inline code examples do not produce false links", () => {
    assert.equal(markdownLinks('Use `[label](missing)` and ``[x](bad)``.').length, 0);
});
test("valid images, encoded paths, headings and directory links pass", async () => {
    const root = await fixture({ "README.md": '[guide](docs/guide%20one.md#start)\n![image](docs/view.png)\n[folder](docs)\n[root](/docs/guide%20one.md#start)', "docs/guide one.md": "# Start", "docs/view.png": "fixture" });
    const result = await checkDocuments(root, ["README.md"]);
    assert.deepEqual(result.issues, []);
    assert.equal(result.localLinks, 4);
});
test("missing files, images and headings fail with source lines", async () => {
    const root = await fixture({ "README.md": '[bad](gone.md)\n![bad](gone.png)\n[bad](guide.md#gone)', "guide.md": "# Here" });
    const result = await checkDocuments(root, ["README.md"]);
    assert.deepEqual(result.issues.map(issue => issue.line), [1, 2, 3]);
    assert.match(result.issues[2].message, /missing heading/);
});
test("same-file and directory README anchors are checked", async () => {
    const root = await fixture({ "README.md": '# Home\n[home](#home)\n[folder](docs#guide)\n[bad](#absent)', "docs/README.md": "Guide\n=====", "other.md": "# Other\n[self](#other)" });
    const result = await checkDocuments(root, ["README.md", "other.md"]);
    assert.equal(result.issues.length, 1);
    assert.match(result.issues[0].message, /#absent/);
});
test("reference definitions and undefined references are checked", async () => {
    const root = await fixture({ "README.md": '[ok][ref]\n[missing][nope]\n[ref]: guide.md#start "Guide"', "guide.md": "# Start" });
    const result = await checkDocuments(root, ["README.md"]);
    assert.equal(result.issues.length, 1);
    assert.match(result.issues[0].message, /undefined reference: nope/);
});
test("HTML image and anchor destinations are checked", async () => {
    const root = await fixture({ "README.md": '<img src="view.png">\n<a href="guide.md#custom">Guide</a>', "view.png": "fixture", "guide.md": '<a id="custom"></a>' });
    assert.deepEqual((await checkDocuments(root, ["README.md"])).issues, []);
});
test("external URLs are counted but never requested", async () => {
    const root = await fixture({ "README.md": '[web](https://invalid.example)\n[mail](mailto:invalid@example.test)\n![cdn](//invalid.example/image.png)' });
    const result = await checkDocuments(root, ["README.md"]);
    assert.equal(result.externalLinks, 3);
    assert.deepEqual(result.issues, []);
});
test("path escapes and malformed encodings fail", async () => {
    const root = await fixture({ "README.md": '[bad](../outside.md)\n[bad](%2e%2e/outside.md)\n[bad](broken%zz.md)' });
    assert.equal((await checkDocuments(root, ["README.md"])).issues.length, 3);
    await assert.rejects(checkDocuments(root, ["../outside.md"]), /escapes repository/);
});
test("symlink escapes are refused before reading external content", async () => {
    const root = await fixture({ "README.md": "[bad](outside/secret.md)" });
    const external = await fixture({ "secret.md": "# Secret" });
    await symlink(external, join(root, "outside"), process.platform === "win32" ? "junction" : "dir");
    const result = await checkDocuments(root, ["README.md"]);
    assert.equal(result.issues.length, 1);
    assert.match(result.issues[0].message, /symlink escapes repository/);
});
