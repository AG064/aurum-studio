// Dependency-free checks for repository-local Markdown links and images.
// This is not a full Markdown renderer or an external URL availability check.
import { readFile, stat, realpath } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { dirname, extname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

function unfence(text) {
    let fence = null;
    return text.split("\n").map(line => {
        const match = line.match(/^ {0,3}(`{3,}|~{3,})/);
        if (!fence && match) fence = { char: match[1][0], length: match[1].length };
        else if (fence && match?.[1][0] === fence.char && match[1].length >= fence.length && line.slice(match[0].length).trim() === "") {
            fence = null;
            return "";
        } else if (!fence) return line;
        return "";
    }).join("\n");
}

function unescape(text) {
    return text.replace(/\\([\\()[\]<> ])/g, "$1");
}

function slug(text) {
    return text.replace(/<[^>]*>/g, "").replace(/!?(?:\[([^\]]+)\])\([^)]*\)/g, "$1")
        .replace(/&amp;/g, "&").toLowerCase()
        .replace(/[^\p{L}\p{N}_\-\s]/gu, "").replace(/\s/g, "-");
}

export function headingAnchors(text) {
    const visible = unfence(text);
    const anchors = new Set();
    for (const match of visible.matchAll(/<(?:a|[a-z][a-z0-9]*)\b[^>]*\b(?:id|name)=["']([^"']+)["'][^>]*>/gi)) anchors.add(match[1]);
    const lines = visible.split("\n");
    for (let i = 0; i < lines.length; i++) {
        const atx = lines[i].match(/^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$/);
        const setext = i + 1 < lines.length && /^ {0,3}(?:=+|-+)\s*$/.test(lines[i + 1]) && lines[i].trim();
        if (!atx && !setext) continue;
        const base = slug(atx ? atx[1] : lines[i].trim());
        let anchor = base, suffix = 0;
        while (anchors.has(anchor)) anchor = `${base}-${++suffix}`;
        anchors.add(anchor);
        if (setext) i++;
    }
    return anchors;
}

export function markdownLinks(text) {
    const visible = unfence(text).replace(/(`+)[\s\S]*?\1/g, match => match.replace(/[^\n]/g, " "));
    const links = [];
    const line = offset => visible.slice(0, offset).split("\n").length;
    const definitions = new Map();
    for (const match of visible.matchAll(/^ {0,3}\[([^\]]+)\]:\s*(<[^>]+>|\S+)/gm)) {
        definitions.set(match[1].trim().toLowerCase(), { target: match[2], line: line(match.index) });
    }
    for (const match of visible.matchAll(/!?\[(?:\\.|[^\]\\])*\]\(\s*(<[^>]*>|(?:\\.|[^\s()]|\([^()]*\))+)\s*(?:["'][^\n]*?["']\s*)?\)/g)) {
        links.push({ target: match[1], line: line(match.index) });
    }
    for (const match of visible.matchAll(/!?\[([^\]]+)\]\[([^\]]*)\]/g)) {
        const key = (match[2] || match[1]).trim().toLowerCase();
        const definition = definitions.get(key);
        links.push(definition ? { target: definition.target, line: line(match.index) } : { error: `undefined reference: ${key}`, line: line(match.index) });
    }
    // Check definitions too, including shortcut-reference destinations.
    links.push(...definitions.values());
    for (const match of visible.matchAll(/<(?:img|a)\b[^>]*\b(?:src|href)=["']([^"']+)["'][^>]*>/gi)) {
        links.push({ target: match[1], line: line(match.index) });
    }
    return links.map(link => link.target ? { ...link, target: unescape(link.target.replace(/^<|>$/g, "")) } : link);
}

function inside(root, path) {
    const part = relative(root, path);
    return !isAbsolute(part) && part !== ".." && !part.startsWith(`..${sep}`);
}

export async function checkDocuments(root, paths) {
    root = await realpath(root);
    const issues = [];
    let localLinks = 0, externalLinks = 0;
    const anchorCache = new Map();
    for (const path of paths) {
        const document = resolve(root, path);
        if (!inside(root, document)) throw new Error(`Document escapes repository: ${path}`);
        if (!inside(root, await realpath(document))) throw new Error(`Document symlink escapes repository: ${path}`);
        const text = await readFile(document, "utf8");
        for (const link of markdownLinks(text)) {
            const issue = message => issues.push({ path, line: link.line, message });
            if (link.error) { issue(link.error); continue; }
            if (/^(?:[a-z][a-z0-9+.-]*:|\/\/)/i.test(link.target)) { externalLinks++; continue; }
            localLinks++;
            const [destination, ...fragments] = link.target.split("#");
            let target, fragment;
            try {
                target = destination ? resolve(destination.startsWith("/") ? root : dirname(document), decodeURIComponent(destination.split("?")[0]).replace(/^\/+/, "")) : document;
                fragment = decodeURIComponent(fragments.join("#"));
            } catch { issue(`invalid URL encoding: ${link.target}`); continue; }
            if (!inside(root, target)) { issue(`link escapes repository: ${link.target}`); continue; }
            try {
                if (!inside(root, await realpath(target))) { issue(`link symlink escapes repository: ${link.target}`); continue; }
                if ((await stat(target)).isDirectory()) {
                    if (!fragment) continue;
                    target = resolve(target, "README.md");
                    if (!inside(root, await realpath(target))) { issue(`README symlink escapes repository: ${link.target}`); continue; }
                }
                if (fragment && extname(target).toLowerCase() === ".md") {
                    if (!anchorCache.has(target)) anchorCache.set(target, headingAnchors(await readFile(target, "utf8")));
                    if (!anchorCache.get(target).has(fragment)) issue(`missing heading: ${link.target}`);
                }
            } catch (error) {
                if (!["ENOENT", "ENOTDIR"].includes(error.code)) throw error;
                issue(`missing destination: ${link.target}`);
            }
        }
    }
    return { documents: paths.length, localLinks, externalLinks, issues };
}

async function main() {
    if (process.argv.length > 3) throw new Error("Usage: node scripts/check-docs.mjs [repository-root]");
    const root = resolve(process.argv[2] || resolve(dirname(fileURLToPath(import.meta.url)), ".."));
    const output = execFileSync("git", ["-C", root, "ls-files", "-z", "--cached", "--others", "--exclude-standard"], { encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
    const paths = [...new Set(output.split("\0").filter(path => /\.md$/i.test(path)))].sort();
    if (!paths.length) throw new Error("No repository Markdown files found");
    const result = await checkDocuments(root, paths);
    for (const issue of result.issues) console.error(`${issue.path}:${issue.line}: ${issue.message}`);
    console.log(`Checked ${result.documents} Markdown files and ${result.localLinks} local destinations; ${result.externalLinks} external links were not fetched.`);
    process.exitCode = result.issues.length ? 1 : 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
    main().catch(error => { console.error(error.message); process.exitCode = 1; });
}
