(function () {
    "use strict";
    const ready = () => {
        const $ = (id) => document.getElementById(id);
        let sceneHash = null,
            sceneFile = "",
            fileHash = null,
            loadedFile = "";
        let selected = null,
            watching = false,
            dialogAction = "create",
            agent = null;
        let activeRoot = "",
            godotFolder = "godot",
            dirty = false,
            draftTimer = null,
            draftStored = true,
            pendingDraft = null;
        const draftId = crypto.randomUUID();
        let saveFailed = false;
        let preview = null,
            liveFile = "",
            liveHash = null,
            liveValues = {},
            liveTimer = null,
            liveBusy = false,
            liveQueued = false;
        const headers = () => {
            const value = { "Content-Type": "application/json" };
            const token = sessionStorage.getItem("aurum_token");
            if (token) value["X-Aurum-Token"] = token;
            return value;
        };
        async function api(path, body) {
            const result = await fetch(path, {
                method: body ? "POST" : "GET",
                headers: headers(),
                ...(body ? { body: JSON.stringify(body) } : {}),
            });
            const text = await result.text();
            let value;
            try {
                value = JSON.parse(text);
            } catch (_) {
                throw new Error(text || "The server returned no result");
            }
            if (!result.ok || value.ok === false) {
                const error = new Error(
                    value.error ||
                        value.report_error ||
                        (
                            value.report?.failures ||
                            value.diagnostics?.errors ||
                            value.errors ||
                            value.log ||
                            []
                        ).join("\n") ||
                        "Operation failed",
                );
                error.result = value;
                throw error;
            }
            return value;
        }
        async function task(label, action) {
            $("workspace-status").textContent = label + "...";
            $("workspace-status").dataset.state = "busy";
            try {
                const result = await action();
                $("workspace-status").textContent = label + " complete";
                $("workspace-status").dataset.state = "ok";
                return result;
            } catch (error) {
                $("workspace-status").textContent = error.message;
                $("workspace-status").dataset.state = "error";
                throw error;
            }
        }
        const bind = (id, label, callback) =>
            $(id).addEventListener("click", async () => {
                const hadFocus = document.activeElement === $(id);
                $(id).disabled = true;
                $(id).setAttribute("aria-busy", "true");
                try {
                    await task(label, callback);
                } catch (_) {
                } finally {
                    $(id).disabled = false;
                    $(id).removeAttribute("aria-busy");
                    if (hadFocus) $(id).focus();
                }
            });
        const operation = (request, project = activeRoot) => {
            if (!project) throw new Error("Choose a project first");
            return api("/api/project", { ...request, project });
        };
        async function showOperation(id, request) {
            $(id).textContent = "Running...";
            const started = performance.now();
            try {
                const result = await operation(request);
                $(id).textContent = JSON.stringify(result, null, 2);
                recordReceipt(request, result, true, started);
                return result;
            } catch (error) {
                const value = error.result || { ok: false, error: error.message };
                $(id).textContent = JSON.stringify(value, null, 2);
                recordReceipt(request, value, false, started);
                throw error;
            }
        }
        async function flushDraft(snapshot = pendingDraft) {
            clearTimeout(draftTimer);
            if (!snapshot) return;
            if (!snapshot.path)
                throw new Error("Name the file to retain its draft");
            await operation(
                { op: "draft_save", ...snapshot },
                snapshot.project,
            );
            if (pendingDraft === snapshot) {
                pendingDraft = null;
                draftStored = true;
                $("draft-status").textContent =
                    "Draft stored. Save to apply it to the project.";
            }
        }
        function queueDraft() {
            saveFailed = false;
            dirty = true;
            draftStored = false;
            $("draft-status").textContent = "Unsaved changes; storing draft...";
            clearTimeout(draftTimer);
            const snapshot = {
                project: activeRoot,
                path: loadedFile || $("file-path").value.trim(),
                text: $("file-editor").value,
                base_sha256: fileHash || "",
                draft_id: draftId,
            };
            pendingDraft = snapshot;
            draftTimer = setTimeout(
                () =>
                    flushDraft(snapshot).catch((error) => {
                        $("draft-status").textContent = error.message;
                    }),
                350,
            );
        }
        $("file-editor").addEventListener("input", queueDraft);
        window.addEventListener("beforeunload", (event) => {
            if (dirty && !draftStored) {
                event.preventDefault();
                event.returnValue = "";
            }
        });
        async function projects() {
            const data = await api("/api/projects");
            const state = await api("/api/state");
            activeRoot = state.root;
            $("run-web").disabled = !activeRoot;
            godotFolder = state.godot_directory || "";
            $("project").textContent = state.project;
            $("project-path").textContent = state.root;
            $("project-path").title = state.root;
            document.querySelectorAll("[data-native]").forEach((button) => {
                button.hidden = !state.native_package;
            });
            const entries = data.projects.slice();
            if (
                !entries.some(
                    (entry) =>
                        entry.path.toLowerCase() ===
                        data.selected.toLowerCase(),
                )
            )
                entries.push({ name: state.project, path: data.selected });
            $("project-select").replaceChildren();
            for (const entry of entries) {
                const option = document.createElement("option");
                option.value = entry.path;
                option.textContent = entry.name;
                option.selected =
                    entry.path.toLowerCase() === data.selected.toLowerCase();
                $("project-select").append(option);
            }
            await api("/api/command", { command: "doctor" });
        }
        async function refreshFiles() {
            const result = await operation({ op: "files" });
            $("file-list").replaceChildren();
            const gamePrefix = godotFolder ? godotFolder + "/" : "";
            const rank = (path) => {
                const relative = path.startsWith(gamePrefix)
                    ? path.slice(gamePrefix.length)
                    : path;
                const priority = [
                    "main.tscn",
                    "main.gd",
                    "hud.gd",
                    "tuning.json",
                ].indexOf(relative);
                return priority >= 0
                    ? priority
                    : path.startsWith(gamePrefix)
                      ? 10
                      : 20;
            };
            const files = result.files
                .filter(
                    (path) =>
                        !path.startsWith("dist/") &&
                        !path.includes("/dist/") &&
                        !/\.(uid|import)$/.test(path),
                )
                .sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
            for (const path of files) {
                const button = document.createElement("button");
                button.title = path;
                button.dataset.path = path;
                const icon = document.createElement("img");
                icon.src = "/icons/file.svg";
                icon.alt = "";
                const label = document.createElement("span");
                label.textContent = path.startsWith(godotFolder + "/")
                    ? path.slice(godotFolder.length + 1)
                    : path;
                button.append(icon, label);
                button.dataset.selected = String(
                    path === loadedFile ||
                        (!loadedFile && path === godotFolder + "/main.tscn"),
                );
                button.addEventListener("click", () =>
                    task("Read file", async () => {
                        $("file-path").value = path;
                        await openFile();
                        setPanel("files");
                    }).catch(() => {}),
                );
                $("file-list").append(button);
            }
            if (!files.length) {
                const note = document.createElement("div");
                note.textContent = "No editable files in this project.";
                $("file-list").append(note);
            }
            liveFile =
                result.files.find(
                    (path) =>
                        path ===
                        (godotFolder ? godotFolder + "/" : "") + "tuning.json",
                ) || "";
            $("live-tuning").hidden = !liveFile;
            if (liveFile && !liveBusy) await loadLiveTuning();
        }
        async function openFile(savedOnly = false) {
            saveFailed = false;
            const path = $("file-path").value.trim(),
                project = activeRoot;
            $("file-editor").disabled = true;
            try {
                await flushDraft();
                const cache = await operation(
                    { op: "draft_read", path },
                    project,
                );
                let result;
                try {
                    result = await operation({ op: "read", path }, project);
                } catch (error) {
                    if (!cache.draft) throw error;
                    result = { text: "", sha256: "" };
                }
                if (
                    activeRoot !== project ||
                    $("file-path").value.trim() !== path
                )
                    return;
                const draft =
                    !savedOnly &&
                    cache.draft &&
                    cache.draft.text !== result.text
                        ? cache.draft
                        : null;
                $("file-editor").value = draft ? draft.text : result.text;
                fileHash = draft ? draft.base_sha256 : result.sha256;
                loadedFile = path;
                dirty = Boolean(draft);
                draftStored = true;
                pendingDraft = null;
                document
                    .querySelectorAll("#file-list button")
                    .forEach((button) => {
                        button.dataset.selected = String(
                            button.dataset.path === path,
                        );
                    });
                $("draft-status").textContent = draft
                    ? "Unsaved draft restored. Save to apply it."
                    : cache.draft
                      ? "Saved version. A retained draft is available."
                      : "Saved version";
            } finally {
                if (activeRoot === project) $("file-editor").disabled = false;
            }
        }
        async function agentConfig() {
            agent = await api("/api/agent");
            if ($("agent-readonly").checked)
                agent.mcpServers.aurum.args.push("--read-only");
            $("agent-config").textContent = JSON.stringify(agent, null, 2);
        }
        function selectNode(node) {
            selected = node;
            document.querySelectorAll("#scene-tree button").forEach((item) =>
                item.setAttribute("aria-current", String(item.dataset.path === node.path)),
            );
            $("selected-node").value = node.path;
            $("node-label").textContent = node.name;
            $("node-type-label").textContent = node.type;
            ["x", "y", "z"].forEach((axis) => {
                $("pos-" + axis).value = node.position?.[axis] || 0;
            });
            $("pos-z").disabled =
                node.position &&
                !Object.prototype.hasOwnProperty.call(node.position, "z");
            $("node-script").value = node.script || "";
        }
        function renderTree(node, container, depth = 0) {
            const button = document.createElement("button");
            button.style.paddingLeft = 12 + depth * 16 + "px";
            const name = document.createElement("span");
            name.textContent = node.name;
            const kind = document.createElement("span");
            kind.className = "node-type";
            kind.textContent = node.type;
            button.dataset.path = node.path;
            button.append(name, kind);
            button.addEventListener("click", () => selectNode(node));
            container.append(button);
            for (const child of node.children || [])
                renderTree(child, container, depth + 1);
        }
        async function inspectScene(preserveSelection = false) {
            const previous = preserveSelection ? $("selected-node").value : ".";
            const project = activeRoot,
                scene = $("scene-path").value.trim();
            const result = await operation(
                { op: "scene_inspect", scene },
                project,
            );
            if (
                activeRoot !== project ||
                $("scene-path").value.trim() !== scene
            )
                return;
            sceneHash = result.sha256;
            sceneFile =
                result.file_path || "godot/" + $("scene-path").value.trim();
            $("scene-tree").replaceChildren();
            renderTree(result.tree, $("scene-tree"));
            const find = (node) =>
                node.path === previous
                    ? node
                    : (node.children || []).map(find).find(Boolean);
            selectNode(find(result.tree) || result.tree);
        }
        async function editScene(operations) {
            if (!sceneHash) throw new Error("Open the scene before editing it");
            const project = activeRoot;
            await operation(
                {
                    op: "scene_edit",
                    scene: $("scene-path").value.trim(),
                    operations,
                    expected_sha256: sceneHash,
                },
                project,
            );
            if (activeRoot === project) await inspectScene(true);
        }
        function setPanel(panel) {
            document
                .querySelectorAll("button[data-panel]")
                .forEach((item) =>
                    item.setAttribute(
                        "aria-selected",
                        String(item.dataset.panel === panel),
                    ),
                );
            ["scene", "files", "agents", "export"].forEach((name) => {
                $("panel-" + name).hidden = name !== panel;
            });
            document.querySelector(".app").dataset.panel = panel;
            document
                .querySelectorAll(".workspace-tabs [role=tab]")
                .forEach((tab) => {
                    tab.tabIndex = tab.dataset.panel === panel ? 0 : -1;
                });
            closeDrawers();
        }
        document.querySelectorAll("button[data-panel]").forEach((button) =>
            button.addEventListener("click", () => {
                setPanel(button.dataset.panel);
                if (button.dataset.panel === "agents")
                    task("Agent configuration", agentConfig).catch(() => {});
            }),
        );
        async function settleLiveEdits() {
            if (previewBusy)
                throw new Error(
                    "Wait for the preview build to finish before changing projects.",
                );
            if (liveBusy)
                throw new Error(
                    "Wait for the live tuning save to finish before changing projects.",
                );
            if (liveTimer !== null) {
                clearTimeout(liveTimer);
                liveTimer = null;
                await applyLive();
            }
            liveQueued = false;
        }
        $("project-select").addEventListener("change", () =>
            task("Select project", async () => {
                $("file-editor").disabled = true;
                try {
                    await settleLiveEdits();
                    await flushDraft();
                } catch (error) {
                    $("file-editor").disabled = false;
                    $("project-select").value = activeRoot;
                    throw error;
                }
                await api("/api/projects", {
                    action: "select",
                    path: $("project-select").value,
                });
                sceneHash = null;
                fileHash = null;
                loadedFile = "";
                watching = false;
                $("develop").textContent = "Develop";
                $("scene-tree").textContent = "Open a scene to inspect it.";
                $("file-editor").value = "";
                $("file-editor").disabled = false;
                dirty = false;
                draftStored = true;
                await stopPreview();
                await projects();
                await agentConfig();
                await refreshFiles();
                await inspectScene();
            }).catch(() => {}),
        );
        function showProjectDialog(action) {
            dialogAction = action;
            $("project-dialog-title").textContent =
                action === "create" ? "New project" : "Import project";
            $("project-name-label").hidden = action !== "create";
            $("project-template-label").hidden = action !== "create";
            $("project-dialog-error").textContent = "";
            $("project-dialog").showModal();
            $("project-folder").focus();
        }
        $("new-project").addEventListener("click", () =>
            showProjectDialog("create"),
        );
        $("import-project").addEventListener("click", () =>
            showProjectDialog("import"),
        );
        $("cancel-project").addEventListener("click", () =>
            $("project-dialog").close(),
        );
        $("project-form").addEventListener("submit", async (event) => {
            event.preventDefault();
            try {
                await settleLiveEdits();
                await flushDraft();
            } catch (error) {
                $("project-dialog-error").textContent = error.message;
                return;
            }
            try {
                await task("Open project", async () => {
                    await api("/api/projects", {
                        action: dialogAction,
                        path: $("project-folder").value.trim(),
                        name: $("project-new-name").value.trim(),
                        template: $("project-template").value,
                    });
                    await projects();
                });
                sceneHash = null;
                fileHash = null;
                loadedFile = "";
                $("file-editor").value = "";
                $("scene-tree").textContent = "Open a scene to inspect it.";
                $("project-dialog").close();
                await stopPreview();
                await refreshFiles();
                await inspectScene();
            } catch (error) {
                $("project-dialog-error").textContent = error.message;
            }
        });
        bind("develop", "Development", async () => {
            await api("/api/command", {
                command: watching ? "end-develop" : "develop",
            });
            watching = !watching;
            $("develop").textContent = watching ? "Stop watching" : "Develop";
        });
        bind("validate-project", "Validation", () => {
            showDock("checks");
            return showOperation("validate-result", { op: "validate" });
        });
        bind("package-project", "Package Windows app", async () => {
            await showOperation("export-result", {
                op: "package",
                output: $("package-path").value.trim(),
            });
        });
        bind("package-web", "Export browser game", async () => {
            const started = performance.now();
            const request = { op: "web_export", output: $("web-package-path").value.trim() };
            let result;
            try {
                result = await api("/api/preview", {
                    action: "export",
                    project: activeRoot,
                    output: request.output,
                });
            } catch (error) {
                $("export-result").textContent = error.message;
                recordReceipt(request, error.result || { ok: false, error: error.message }, false, started);
                throw error;
            }
            $("export-result").textContent =
                `${result.files} files exported to ${result.directory}\n\n${result.message}`;
            recordReceipt(request, result, true, started);
        });
        bind("refresh-presets", "Read export presets", async () => {
            $("export-result").textContent = JSON.stringify(
                await operation({ op: "presets" }),
                null,
                2,
            );
        });
        bind("configure-export", "Configure export", async () => {
            const result = await operation({
                op: "configure_export",
                platform: $("export-platform").value,
            });
            $("export-preset").value = result.preset;
            $("export-result").textContent = JSON.stringify(result, null, 2);
        });
        bind("export-project", "Platform export", () =>
            showOperation("export-result", {
                op: "export",
                preset: $("export-preset").value.trim(),
                output: $("platform-output").value.trim(),
                debug: $("export-debug").checked,
            }),
        );
        async function playtest(rendered = false) {
            let args;
            try {
                args = JSON.parse($("test-args").value);
            } catch (error) {
                throw new Error("Game arguments must be a JSON array of strings (" + error.message + ")");
            }
            if (!Array.isArray(args))
                throw new Error(
                    "Game arguments must be a JSON array of strings",
                );
            const report = $("test-report").checked;
            const request = {
                op: rendered ? "capture" : "play",
                frames: $("test-frames").value ? Number($("test-frames").value) : report || args.length ? 36000 : 120,
                timeout_seconds: Number($("test-timeout").value),
                fixed_fps: 60,
                user_args: args,
                report,
            };
            const scene = $("test-scene").value.trim();
            if (scene) request.scene = scene;
            await showOperation("test-result", request);
        }
        bind("headless-play", "Headless playtest", () => playtest(false));
        bind("capture-playtest", "Rendered playtest", () => playtest(true));
        bind("refresh-files", "Read files", refreshFiles);
        bind("open-file", "Read file", openFile);
        bind("open-saved-file", "Read saved version", () => openFile(true));
        bind("restore-draft", "Restore draft", () => openFile(false));
        bind("new-file", "New file", async () => {
            await flushDraft();
            loadedFile = "";
            fileHash = null;
            dirty = false;
            draftStored = true;
            $("file-path").value =
                (godotFolder ? godotFolder + "/" : "") + "new_script.gd";
            $("file-editor").value = "";
            $("file-editor").disabled = false;
            $("draft-status").textContent = "New file";
            $("file-path").focus();
        });
        bind("save-file", "Save file", async () => {
            const path = $("file-path").value.trim(),
                project = activeRoot;
            if (path !== loadedFile && fileHash)
                throw new Error("Open the selected path before overwriting it");
            clearTimeout(draftTimer);
            $("file-editor").disabled = true;
            $("undo-file").disabled = true;
            try {
                await operation(
                    {
                        op: "write",
                        path,
                        text: $("file-editor").value,
                        expected_sha256: path === loadedFile ? fileHash : "",
                    },
                    project,
                );
                pendingDraft = null;
                await operation(
                    { op: "draft_clear", path, draft_id: draftId },
                    project,
                );
                if (activeRoot === project) {
                    dirty = false;
                    draftStored = true;
                    await openFile(true);
                    await refreshFiles();
                }
            } catch (error) {
                // A refused save (for example the file changed on disk) must
                // never cost the edit: store the draft before reporting.
                saveFailed = true;
                const stored = await flushDraft().then(
                    () => true,
                    () => false,
                );
                $("draft-status").textContent =
                    "Save refused: " + error.message + " " +
                    (stored
                        ? "Your draft is stored. Load the saved version to compare, then Restore draft."
                        : "The draft could not be stored; keep this tab open.");
                throw error;
            } finally {
                if (activeRoot === project) $("file-editor").disabled = false;
                $("undo-file").disabled = false;
            }
        });
        bind("undo-file", "Undo file", async () => {
            await operation({
                op: "undo",
                path: $("file-path").value.trim(),
                expected_sha256: fileHash,
            });
            await openFile(true);
        });
        bind("inspect-scene", "Open scene", inspectScene);
        bind("set-main-scene", "Set start scene", () =>
            operation({
                op: "set_main_scene",
                scene: $("scene-path").value.trim(),
            }),
        );
        bind("create-scene", "Create scene", async () => {
            await operation({
                op: "scene_create",
                scene: $("scene-path").value.trim(),
                name: "Main",
                root_type: "Node3D",
            });
            await inspectScene();
        });
        bind("undo-scene", "Undo scene", async () => {
            if (!sceneFile) throw new Error("Open a scene first");
            await operation({
                op: "undo",
                path: sceneFile,
                expected_sha256: sceneHash,
            });
            await inspectScene();
        });
        bind("add-node", "Add node", () => {
            const type = $("node-type").value;
            const properties =
                type === "MeshInstance3D"
                    ? { mesh: { resource: "BoxMesh" } }
                    : {};
            return editScene([
                {
                    op: "create",
                    parent: $("selected-node").value,
                    name: $("node-name").value,
                    type,
                    properties,
                },
            ]);
        });
        bind("apply-position", "Move node", () => {
            const position = {
                x: Number($("pos-x").value),
                y: Number($("pos-y").value),
            };
            if (!$("pos-z").disabled) position.z = Number($("pos-z").value);
            return editScene([
                {
                    op: "set",
                    node: $("selected-node").value,
                    properties: { position },
                },
            ]);
        });
        bind("apply-properties", "Set properties", () =>
            editScene([
                {
                    op: "set",
                    node: $("selected-node").value,
                    properties: JSON.parse($("node-properties").value),
                },
            ]),
        );
        bind("attach-script", "Attach script", () =>
            editScene([
                {
                    op: "attach_script",
                    node: $("selected-node").value,
                    script: $("node-script").value,
                },
            ]),
        );
        bind("remove-node", "Remove node", () => {
            if (!selected || selected.path === ".")
                throw new Error("Select a child node");
            return editScene([{ op: "remove", node: selected.path }]);
        });
        bind("copy-agent", "Copy configuration", async () => {
            await agentConfig();
            await navigator.clipboard.writeText(JSON.stringify(agent, null, 2));
        });
        $("agent-readonly").addEventListener("change", () =>
            agentConfig().catch(() => {}),
        );
        let previewBusy = false, previewPollBusy = false, attemptedRevision = "";
        let externalCheckpoint = null;
        const runtimePending = new Map();
        function runtimeRequest(request) {
            if (!preview) return Promise.reject(new Error("No preview is running"));
            const id = crypto.randomUUID().replaceAll("-", "");
            return new Promise((resolve, reject) => {
                const timer = setTimeout(() => { runtimePending.delete(id); reject(new Error("Runtime response timed out")); }, 5000);
                runtimePending.set(id, { resolve, reject, timer, session: preview.session });
                $("preview-frame").contentWindow.postMessage({ type: "aurum-runtime-request", session: preview.session, id, request }, preview.origin);
            });
        }
        async function restoreWhenReady(checkpoint) {
            const deadline = Date.now() + 30000;
            while (Date.now() < deadline) {
                const probe = await runtimeRequest({ op: "inspect" }).catch(() => ({ ok: false }));
                if (probe.ok) {
                    if (!checkpoint) { $("reload-status").textContent = "Started a fresh preview."; return; }
                    const restored = await runtimeRequest({ op: "restore", checkpoint });
                    if (!restored.ok) throw new Error(restored.error || "Checkpoint restore failed");
                    $("reload-status").textContent = restored.complete
                        ? "Rebuilt and restored the checkpoint."
                        : `Rebuilt with partial state restoration (${restored.skipped} unsupported or changed entries).`;
                    return;
                }
                await new Promise((resolve) => setTimeout(resolve, 250));
            }
            throw new Error("The new game did not make its runtime bridge ready");
        }
        async function adoptPreview(result, checkpoint) {
            const url = new URL(result.url);
            if (url.protocol !== "http:" || url.hostname !== "127.0.0.1") throw new Error("The server returned an invalid preview origin");
            const previous = preview;
            const previousFrame = $("preview-frame");
            const unchanged = previousFrame.src === result.url;
            let nextFrame = previousFrame;
            if (!unchanged) {
                $("reload-status").textContent = checkpoint ? "Restoring checkpoint..." : "Starting a fresh preview...";
                nextFrame = previousFrame.cloneNode(false);
                previousFrame.id = "preview-frame-previous";
                previousFrame.hidden = true;
                nextFrame.id = "preview-frame";
                nextFrame.src = result.url;
                previousFrame.before(nextFrame);
            }
            preview = { ...result, origin: url.origin };
            $("preview-frame").hidden = false;
            $("preview-empty").hidden = true;
            $("preview-revision").textContent = `Source ${(result.source_sha256 || "").slice(0, 8)}`;
            $("preview-session").textContent = "Isolated browser preview";
            if (!unchanged) {
                try {
                    $("preview-state").textContent = "Loading game";
                    await restoreWhenReady(checkpoint);
                    await api("/api/preview", { action: "commit", session: result.session });
                    previousFrame.remove();
                } catch (error) {
                    nextFrame.remove();
                    previousFrame.id = "preview-frame";
                    previousFrame.hidden = !previous;
                    preview = previous;
                    if (previous) await api("/api/preview", { action: "rollback", session: result.session }).catch(() => {});
                    if (previous && checkpoint) await runtimeRequest({ op: "resume", paused: checkpoint.paused }).catch(() => {});
                    $("preview-state").textContent = previous ? "Preview retained" : "Preview unavailable";
                    if (previous) $("preview-revision").textContent = `Source ${(previous.source_sha256 || "").slice(0, 8)}`;
                    throw error;
                }
            }
            else if (checkpoint) await runtimeRequest({ op: "resume", paused: checkpoint.paused });
        }
        async function captureBeforeRebuild() {
            if (!preview || !$("preserve-state").checked) return null;
            const saved = await runtimeRequest({ op: "checkpoint", freeze: true });
            if (!saved.ok) throw new Error(saved.error || "Could not capture runtime state. Turn off state preservation only if a fresh run is intended.");
            return saved.checkpoint;
        }
        async function startPreview(force = false) {
            if (previewBusy)
                throw new Error("A preview build is already in progress.");
            if (!activeRoot) throw new Error("Open a project before starting a preview");
            previewBusy = true;
            const project = activeRoot;
            const previousState = $("preview-state").textContent;
            let checkpoint = null;
            $("reload-status").textContent = preview && $("preserve-state").checked ? "Capturing a checkpoint..." : "Starting a fresh preview...";
            $("run-web").disabled = true;
            $("preview-state").textContent = "Building preview";
            $("preview-detail").textContent =
                "Importing and exporting a private project snapshot. The first build can take a moment.";
            try {
                checkpoint = await captureBeforeRebuild();
                $("reload-status").textContent = checkpoint ? "Checkpoint captured. Building private snapshot..." : "Building private snapshot...";
                const result = await api("/api/preview", { project, force });
                if (activeRoot !== project) return;
                await adoptPreview(result, checkpoint);
                if (result.reused) $("preview-state").textContent = previousState;
            } catch (error) {
                if (checkpoint) await runtimeRequest({ op: "resume", paused: checkpoint.paused }).catch(() => {});
                $("preview-state").textContent = preview
                    ? "Preview retained"
                    : "Preview unavailable";
                $("preview-detail").textContent = error.message;
                $("reload-status").textContent = preview
                    ? `Rebuild failed. ${error.message} Previous run retained.`
                    : `Preview unavailable. ${error.message}`;
                throw error;
            } finally {
                previewBusy = false;
                $("run-web").disabled = false;
            }
        }
        setInterval(async () => {
            if (!preview || previewBusy || previewPollBusy) return;
            previewPollBusy = true;
            try {
                const status = await api("/api/preview");
                if (status.active === false) { await stopPreview(false); return; }
                if (!preview || status.project !== activeRoot) return;
                if (status.building) {
                    if (!externalCheckpoint && $("preserve-state").checked) {
                        const checkpoint = await captureBeforeRebuild();
                        if (checkpoint) externalCheckpoint = { session: preview.session, checkpoint };
                    }
                    $("reload-status").textContent = externalCheckpoint ? "Agent build in progress. Existing run frozen." : "Agent build in progress.";
                    return;
                }
                if (status.session !== preview.session) {
                    previewBusy = true;
                    try {
                        const checkpoint = externalCheckpoint?.session === preview.session ? externalCheckpoint.checkpoint : await captureBeforeRebuild();
                        externalCheckpoint = null;
                        await adoptPreview(status, checkpoint);
                    } finally { previewBusy = false; }
                } else if (externalCheckpoint?.session === preview.session) {
                    const checkpoint = externalCheckpoint.checkpoint;
                    externalCheckpoint = null;
                    await runtimeRequest({ op: "resume", paused: checkpoint.paused });
                    $("reload-status").textContent = "Agent build finished. Existing preview resumed.";
                }
                $("preview-revision").textContent = status.stale ? "Source changed" : `Source ${(status.source_sha256 || "").slice(0, 8)}`;
                if (status.stale && $("auto-rebuild").checked && attemptedRevision !== status.current_sha256) {
                    attemptedRevision = status.current_sha256;
                    await startPreview(false);
                }
            } catch (error) { $("reload-status").textContent = preview ? `${error.message} Previous run retained.` : error.message; }
            finally { previewPollBusy = false; }
        }, 2000);
        bind("inspect-runtime", "Inspect live", async () => {
            $("runtime-status").textContent = "Inspecting live values...";
            const result = await runtimeRequest({ op: "inspect", path: $("runtime-path").value.trim() || "." });
            if (!result.ok) throw new Error(result.error);
            $("runtime-properties").replaceChildren();
            for (const property of result.properties.slice(0, 64)) {
                const label = document.createElement("label");
                label.textContent = property.name;
                const input = document.createElement("input");
                input.setAttribute("aria-label", `Live ${property.name}`);
                input.value = JSON.stringify(property.value);
                input.addEventListener("change", async () => {
                    try {
                        const value = JSON.parse(input.value);
                        const applied = await runtimeRequest({ op: "apply", changes: [{ path: result.path, property: property.name, value }] });
                        if (!applied.ok) throw new Error(applied.error);
                        property.value = value;
                        $("runtime-status").textContent = `${property.name} applied without restarting.`;
                    } catch (error) { $("runtime-status").textContent = error.message; input.value = JSON.stringify(property.value); }
                });
                label.append(input);
                $("runtime-properties").append(label);
            }
            $("runtime-status").textContent = `${result.class}: ${result.properties.length} editable values. Children: ${result.children.map((node) => node.path).join(", ") || "none"}`;
        });
        async function stopPreview(notifyServer = true) {
            if (preview && notifyServer) await api("/api/preview", { action: "stop" });
            preview = null;
            externalCheckpoint = null;
            for (const pending of runtimePending.values()) { clearTimeout(pending.timer); pending.reject(new Error("Preview stopped")); }
            runtimePending.clear();
            $("preview-frame").removeAttribute("src");
            $("preview-frame").hidden = true;
            $("preview-empty").hidden = false;
            $("preview-state").textContent = "Web preview";
            $("preview-session").textContent = "No preview running";
            $("live-runtime-state").textContent = "";
            delete $("live-runtime-state").dataset.snapshot;
            $("preview-revision").textContent = "";
            $("reload-status").textContent = "";
            $("preview-detail").textContent =
                "The preview origin is separate from Studio's control API.";
        }
        function liveStatus(text, error = false) {
            $("live-status").textContent = text;
            $("live-status").dataset.state = error ? "error" : "ready";
        }
        async function loadLiveTuning() {
            if (!liveFile) return;
            const result = await operation({ op: "read", path: liveFile });
            const values = JSON.parse(result.text);
            if (!values || typeof values !== "object" || Array.isArray(values))
                throw new Error("The tuning file must contain an object");
            liveValues = values;
            liveHash = result.sha256;
            $("live-speed").value = Number(values.player_speed ?? 7.6);
            $("live-speed-value").value = Number($("live-speed").value).toFixed(
                1,
            );
            $("live-spawn").value = Number(values.spawn_interval ?? 1.1);
            $("live-damage").value = Number(values.damage_multiplier ?? 1);
            liveStatus(
                "Ready to edit. Values are saved with concurrency checks.",
            );
        }
        async function applyLive() {
            if (liveBusy) {
                liveQueued = true;
                return;
            }
            if (!liveFile || !liveHash)
                throw new Error(
                    "Reload the tuning file before applying changes",
                );
            const values = {
                ...liveValues,
                player_speed: Number($("live-speed").value),
                spawn_interval: Number($("live-spawn").value),
                damage_multiplier: Number($("live-damage").value),
            };
            if (
                !Number.isFinite(values.player_speed) ||
                values.player_speed < 3 ||
                values.player_speed > 14 ||
                !Number.isFinite(values.spawn_interval) ||
                values.spawn_interval < 0.3 ||
                values.spawn_interval > 3 ||
                !Number.isFinite(values.damage_multiplier) ||
                values.damage_multiplier < 0.25 ||
                values.damage_multiplier > 4
            )
                throw new Error(
                    "Tuning values are outside the supported range",
                );
            const project = activeRoot,
                path = liveFile;
            liveBusy = true;
            liveStatus("Saving tuning...");
            try {
                const result = await operation(
                    {
                        op: "write",
                        path,
                        text: JSON.stringify(values, null, 2) + "\n",
                        expected_sha256: liveHash,
                    },
                    project,
                );
                if (project !== activeRoot) return;
                liveHash = result.sha256;
                liveValues = values;
                liveStatus(
                    preview
                        ? "Saved. Waiting for the game to acknowledge the update."
                        : "Saved. Run a compatible project to apply changes live.",
                );
            } catch (error) {
                liveHash = null;
                liveQueued = false;
                $("auto-live").checked = false;
                liveStatus(
                    error.message +
                        " Reload the tuning values before saving again.",
                    true,
                );
                throw error;
            } finally {
                liveBusy = false;
                if (liveQueued) {
                    liveQueued = false;
                    applyLive().catch((error) =>
                        liveStatus(error.message, true),
                    );
                }
            }
        }
        function queueLive() {
            $("live-speed-value").value = Number($("live-speed").value).toFixed(
                1,
            );
            clearTimeout(liveTimer);
            liveTimer = null;
            if ($("auto-live").checked)
                liveTimer = setTimeout(() => {
                    liveTimer = null;
                    applyLive().catch((error) =>
                        liveStatus(error.message, true),
                    );
                }, 350);
        }
        ["live-speed", "live-spawn", "live-damage"].forEach((id) =>
            $(id).addEventListener("input", queueLive),
        );
        bind("apply-live", "Live tuning", applyLive);
        bind("refresh-live", "Reload tuning", loadLiveTuning);
        bind("run-web", "Web preview", () => startPreview(false));
        bind("rebuild-preview", "Rebuild preview", () => startPreview(true));
        bind("stop-preview", "Stop preview", async () => {
            await stopPreview();
            await api("/api/command", { command: "stop" });
        });
        $("preview-size").addEventListener("change", () => {
            $("preview-surface").dataset.size = $("preview-size").value;
        });
        $("fullscreen-preview").addEventListener("click", () => {
            $("preview-surface")
                .requestFullscreen?.()
                .catch((error) => {
                    $("workspace-status").textContent = error.message;
                    $("workspace-status").dataset.state = "error";
                });
        });
        $("toggle-inspector").addEventListener("click", () => {
            const open = document
                .querySelector(".app")
                .classList.toggle("inspector-open");
            $("toggle-inspector").setAttribute("aria-expanded", String(open));
        });
        window.addEventListener("message", (event) => {
            if (
                !preview ||
                event.origin !== preview.origin ||
                event.source !== $("preview-frame").contentWindow
            )
                return;
            const data = event.data;
            if (
                !data ||
                typeof data !== "object" ||
                data.session !== preview.session
            )
                return;
            if (data.type === "aurum-preview-ready") {
                if ($("preview-state").textContent === "Loading game")
                    $("preview-state").textContent = "Preview ready";
                return;
            }
            if (data.type === "aurum-runtime-response") {
                const pending = runtimePending.get(data.id);
                if (pending && pending.session === data.session && data.response && typeof data.response === "object" && JSON.stringify(data.response).length <= 1048576) {
                    clearTimeout(pending.timer);
                    runtimePending.delete(data.id);
                    pending.resolve(data.response);
                }
                return;
            }
            if (
                data.type === "aurum-preview-state" &&
                data.state &&
                typeof data.state === "object"
            ) {
                const state = data.state;
                const phase = String(state.phase || "").slice(0, 30);
                $("preview-state").textContent =
                    phase === "menu"
                        ? "Ready"
                        : phase === "playing"
                          ? "Running"
                          : phase;
                $("live-runtime-state").textContent =
                    `${phase}  |  wave ${Number(state.wave) || 0}  |  hull ${Math.round(Number(state.health) || 0)}`;
                const snapshot = { phase };
                for (const key of [
                    "wave",
                    "health",
                    "kills",
                    "time",
                    "x",
                    "z",
                    "speed",
                    "damage_multiplier",
                    "hot_reloads",
                    "dash_cooldown",
                    "dashes",
                    "credits",
                    "workshops",
                    "upgrades",
                    "view_width",
                    "view_height",
                ]) {
                    const value = Number(state[key]);
                    snapshot[key] = Number.isFinite(value)
                        ? Math.max(-1e9, Math.min(1e9, value))
                        : 0;
                }
                snapshot.revision =
                    typeof state.revision === "string"
                        ? state.revision.slice(0, 64)
                        : "";
                snapshot.weapon =
                    typeof state.weapon === "string"
                        ? state.weapon.slice(0, 24)
                        : "";
                snapshot.choices = Array.isArray(state.choices)
                    ? state.choices
                          .slice(0, 3)
                          .filter((value) => typeof value === "string")
                          .map((value) => value.slice(0, 32))
                    : [];
                $("live-runtime-state").dataset.snapshot =
                    JSON.stringify(snapshot);
            } else if (
                data.type === "aurum-preview-tuning" &&
                data.sha256 === liveHash
            ) {
                liveStatus("Applied to the running game. Run state preserved.");
            } else if (data.type === "aurum-preview-error") {
                liveStatus(
                    String(data.message || "Preview error").slice(0, 500),
                    true,
                );
            }
        });
        document.querySelectorAll(".menu-surface button").forEach((button) =>
            button.addEventListener("click", () => {
                document.querySelector(".app-menu").open = false;
            }),
        );
        window.addEventListener("keydown", (event) => {
            if (
                (event.ctrlKey || event.metaKey) &&
                event.key.toLowerCase() === "s" &&
                !$("panel-files").hidden
            ) {
                event.preventDefault();
                $("save-file").click();
            }
        });
        $("shutdown").addEventListener(
            "click",
            (event) => {
                event.preventDefault();
                event.stopImmediatePropagation();
                task("Close Studio", async () => {
                    await flushDraft();
                    await api("/api/stop", {});
                }).catch(() => {});
            },
            true,
        );
        // ---- Studio shell: menus, tabs, drawers, dock, editor, receipts ----
        const app = document.querySelector(".app");
        const menu = document.querySelector(".app-menu");
        function closeDrawers() {
            app.classList.remove("explorer-open", "inspector-open");
            $("toggle-explorer").setAttribute("aria-expanded", "false");
            $("toggle-inspector").setAttribute("aria-expanded", "false");
        }
        $("toggle-explorer").addEventListener("click", () => {
            const open = !app.classList.contains("explorer-open");
            closeDrawers();
            app.classList.toggle("explorer-open", open);
            $("toggle-explorer").setAttribute("aria-expanded", String(open));
        });
        $("toggle-inspector").addEventListener("click", () => {
            if (app.classList.contains("inspector-open")) {
                app.classList.remove("explorer-open");
                $("toggle-explorer").setAttribute("aria-expanded", "false");
            }
        });
        // Run and Rebuild are explicit requests to look at the preview. Automatic
        // rebuilds must not move someone away from the file they are editing.
        ["run-web", "rebuild-preview"].forEach((id) =>
            $(id).addEventListener("click", () => setPanel("scene")),
        );

        const menuItems = () =>
            [...menu.querySelectorAll("button, a")].filter((item) => !item.hidden);
        document.addEventListener("click", (event) => {
            if (menu.open && !menu.contains(event.target)) menu.open = false;
        });
        menu.addEventListener("keydown", (event) => {
            const items = menuItems();
            const index = items.indexOf(document.activeElement);
            const summary = menu.querySelector("summary");
            let next = null;
            if (event.key === "ArrowDown")
                next = items[(index + 1) % items.length];
            else if (event.key === "ArrowUp")
                next = items[(index <= 0 ? items.length : index) - 1];
            else if (event.key === "Home") next = items[0];
            else if (event.key === "End") next = items[items.length - 1];
            if (!next) return;
            event.preventDefault();
            if (!menu.open) menu.open = true;
            next.focus();
        });
        window.addEventListener("keydown", (event) => {
            if (event.key !== "Escape" || event.defaultPrevented) return;
            if (menu.open) {
                menu.open = false;
                menu.querySelector("summary").focus();
            } else if (app.classList.contains("explorer-open")) {
                closeDrawers();
                $("toggle-explorer").focus();
            } else if (app.classList.contains("inspector-open")) {
                closeDrawers();
                $("toggle-inspector").focus();
            }
        });

        function tabKeys(list, onSelect) {
            list.addEventListener("keydown", (event) => {
                const tabs = [...list.querySelectorAll("[role=tab]")];
                const index = tabs.indexOf(document.activeElement);
                if (index < 0) return;
                let next = null;
                if (event.key === "ArrowRight" || event.key === "ArrowDown")
                    next = tabs[(index + 1) % tabs.length];
                else if (event.key === "ArrowLeft" || event.key === "ArrowUp")
                    next = tabs[(index + tabs.length - 1) % tabs.length];
                else if (event.key === "Home") next = tabs[0];
                else if (event.key === "End") next = tabs[tabs.length - 1];
                if (!next) return;
                event.preventDefault();
                next.focus();
                onSelect(next);
            });
        }
        tabKeys(document.querySelector(".workspace-tabs"), (tab) => tab.click());

        function showDock(name) {
            document.querySelectorAll("[data-dock]").forEach((tab) => {
                const on = tab.dataset.dock === name;
                tab.setAttribute("aria-selected", String(on));
                tab.tabIndex = on ? 0 : -1;
                $("dock-" + tab.dataset.dock).hidden = !on;
            });
            setDock(true);
        }
        function setDock(open) {
            $("dock").dataset.open = String(open);
            $("toggle-dock").setAttribute("aria-expanded", String(open));
            $("toggle-dock").textContent = open ? "Collapse" : "Expand";
        }
        document
            .querySelectorAll("[data-dock]")
            .forEach((tab) =>
                tab.addEventListener("click", () => showDock(tab.dataset.dock)),
            );
        tabKeys(document.querySelector(".dock-tabs"), (tab) =>
            showDock(tab.dataset.dock),
        );
        $("toggle-dock").addEventListener("click", () =>
            setDock($("dock").dataset.open !== "true"),
        );

        // Error notices stay until acknowledged or replaced by the next task.
        const notice = $("workspace-status");
        new MutationObserver(() => {
            $("dismiss-status").hidden = notice.dataset.state !== "error";
        }).observe(notice, { attributes: true, attributeFilter: ["data-state"] });
        $("dismiss-status").addEventListener("click", () => {
            notice.dataset.state = "ok";
            notice.textContent = "Ready";
        });

        // Preview state colour comes from the text the real state machine wrote.
        const previewState = $("preview-state");
        const tone = () => {
            const text = previewState.textContent;
            previewState.dataset.tone = /^(Running|Ready|Preview ready)$/.test(text)
                ? "live"
                : /Building|Loading|Starting/.test(text)
                  ? "busy"
                  : /unavailable/.test(text)
                    ? "bad"
                    : "idle";
        };
        new MutationObserver(tone).observe(previewState, {
            childList: true,
            characterData: true,
            subtree: true,
        });
        tone();

        // Editor chrome: line numbers, cursor position, base hash, draft state.
        const editor = $("file-editor");
        const gutter = $("line-gutter");
        function editorPosition() {
            const before = editor.value.slice(0, editor.selectionStart);
            const line = before.split("\n").length;
            const column = before.length - before.lastIndexOf("\n");
            $("editor-pos").textContent = `Ln ${line}, Col ${column}`;
        }
        function syncEditor() {
            const lines = editor.value.split("\n").length;
            if (gutter.dataset.lines !== String(lines)) {
                const numbers = [];
                for (let i = 1; i <= Math.min(lines, 50000); i++) numbers.push(i);
                gutter.textContent = numbers.join("\n");
                gutter.dataset.lines = String(lines);
            }
            gutter.scrollTop = editor.scrollTop;
            $("editor-file").textContent =
                loadedFile || $("file-path").value.trim() || "No file";
            $("editor-base").textContent =
                "Base " + (fileHash ? fileHash.slice(0, 8) : "none");
            document.querySelector(".draft-row").dataset.state = saveFailed
                ? "error"
                : dirty
                  ? draftStored
                      ? "stored"
                      : "pending"
                  : "clean";
            $("editor-dirty").hidden = !dirty;
            editorPosition();
        }
        // Every path that loads or changes the editor also writes the draft
        // status line, so one observer keeps the chrome in step with it.
        new MutationObserver(syncEditor).observe($("draft-status"), {
            childList: true,
            characterData: true,
            subtree: true,
        });
        editor.addEventListener("input", syncEditor);
        editor.addEventListener("scroll", () => {
            gutter.scrollTop = editor.scrollTop;
        });
        ["keyup", "click", "select"].forEach((name) =>
            editor.addEventListener(name, editorPosition),
        );

        // Receipts: what ran in this session, how it ended, how long it took.
        const MAX_RECEIPTS = 20;
        function receiptFacts(value) {
            const facts = [];
            if (!value || typeof value !== "object") return facts;
            for (const [key, item] of Object.entries(value)) {
                if (key === "text" || facts.length >= 10) continue;
                if (["string", "number", "boolean"].includes(typeof item))
                    facts.push([key, String(item).slice(0, 240)]);
                else if (Array.isArray(item) && item.every((x) => typeof x === "string"))
                    facts.push([key, item.length ? item.slice(0, 5).join("; ").slice(0, 240) : "none"]);
            }
            return facts;
        }
        function recordReceipt(request, value, ok, started) {
            const item = document.createElement("li");
            const details = document.createElement("details");
            const summary = document.createElement("summary");
            const time = document.createElement("span");
            time.className = "r-time";
            time.textContent = new Date().toLocaleTimeString([], { hour12: false });
            const op = document.createElement("span");
            op.className = "r-op";
            op.textContent = String(request.op || "operation");
            const verdict = document.createElement("span");
            verdict.className = "r-verdict " + (ok ? "ok" : "failed");
            verdict.textContent = ok ? "returned ok" : "failed";
            const duration = document.createElement("span");
            duration.className = "r-dur";
            duration.title = "Measured in this page, including network time";
            duration.textContent = Math.round(performance.now() - started) + " ms";
            summary.append(time, op, verdict, duration);
            const facts = document.createElement("dl");
            const asked = { ...request };
            delete asked.text;
            for (const [key, text] of [["request", JSON.stringify(asked)], ...receiptFacts(value)]) {
                const dt = document.createElement("dt");
                dt.textContent = key;
                const dd = document.createElement("dd");
                dd.textContent = text;
                facts.append(dt, dd);
            }
            const raw = document.createElement("pre");
            raw.textContent = JSON.stringify(value, null, 2).slice(0, 20000);
            details.append(summary, facts, raw);
            item.append(details);
            $("receipts-list").prepend(item);
            while ($("receipts-list").childElementCount > MAX_RECEIPTS)
                $("receipts-list").lastElementChild.remove();
            const count = $("receipts-list").childElementCount;
            $("receipt-count").textContent = String(count);
            $("receipt-count").hidden = false;
            $("receipts-empty").hidden = true;
            $("clear-receipts").hidden = false;
        }
        $("clear-receipts").addEventListener("click", () => {
            $("receipts-list").replaceChildren();
            $("receipt-count").hidden = true;
            $("receipts-empty").hidden = false;
            $("clear-receipts").hidden = true;
        });

        // Operation contract: read-only discovery from the shared registry.
        function contractRows(result) {
            const source =
                result.operations ?? result.catalog ?? result.ops ?? null;
            const entries = Array.isArray(source)
                ? source
                : source && typeof source === "object"
                  ? Object.entries(source).map(([key, entry]) =>
                        entry && typeof entry === "object" ? { name: key, ...entry } : { name: key },
                    )
                  : [];
            return entries
                .filter((entry) => entry && typeof entry === "object")
                .map((entry) => {
                    const access =
                        entry.access ??
                        entry.classification ??
                        entry.class ??
                        entry.kind ??
                        (typeof entry.read_only === "boolean"
                            ? entry.read_only ? "read" : "write"
                            : typeof entry.write === "boolean"
                              ? entry.write ? "write" : "read"
                              : "");
                    const required = entry.required ?? entry.required_fields ?? [];
                    return {
                        name: String(entry.op ?? entry.name ?? entry.operation ?? ""),
                        access: String(access),
                        required: Array.isArray(required) ? required.join(", ") : String(required),
                        summary: String(entry.summary ?? entry.description ?? ""),
                    };
                })
                .filter((row) => row.name);
        }
        bind("describe-op", "Read contract", async () => {
            const name = $("describe-name").value.trim();
            const request = name ? { op: "describe", operation: name } : { op: "describe" };
            const list = $("contract-list");
            list.replaceChildren();
            $("contract-raw").hidden = true;
            let result;
            try {
                result = await operation(request);
            } catch (error) {
                const note = document.createElement("p");
                note.className = "error";
                note.textContent = "Contract unavailable: " + error.message;
                list.append(note);
                throw error;
            }
            const rows = contractRows(result);
            if (rows.length) {
                const table = document.createElement("table");
                const head = table.createTHead().insertRow();
                for (const title of ["Operation", "Access", "Required", "Note"]) {
                    const cell = document.createElement("th");
                    cell.textContent = title;
                    head.append(cell);
                }
                const body = table.createTBody();
                for (const row of rows) {
                    const tr = body.insertRow();
                    for (const key of ["name", "access", "required", "summary"]) {
                        const td = tr.insertCell();
                        td.textContent = row[key];
                        if (key === "access" && /write/i.test(row.access)) td.className = "write";
                    }
                }
                list.append(table);
            } else {
                $("contract-raw").textContent = JSON.stringify(result, null, 2);
                $("contract-raw").hidden = false;
            }
        });

        task("Load workspace", async () => {
            await projects();
            await agentConfig();
            await refreshFiles();
            await inspectScene();
        }).catch(() => {
            if ($("file-list").textContent.trim() === "Loading files") {
                $("file-list").textContent =
                    "Files could not be loaded. Use refresh once the problem in the status message is fixed.";
            }
        });
    };
    if (document.readyState === "loading")
        document.addEventListener("DOMContentLoaded", ready);
    else ready();
})();
