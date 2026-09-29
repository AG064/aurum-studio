// This script runs inside the game origin, never in Studio's privileged origin.
(function () {
    let applied = "",
        sent = "",
        pending = false;
    const tell = (type, detail = {}) =>
        window.parent.postMessage(
            { type, session: AURUM_PREVIEW.session, ...detail },
            AURUM_PREVIEW.parent,
        );
    window.addEventListener("error", (event) =>
        tell("aurum-preview-error", {
            message: String(event.message).slice(0, 500),
        }),
    );
    window.addEventListener("unhandledrejection", (event) =>
        tell("aurum-preview-error", {
            message: String(event.reason).slice(0, 500),
        }),
    );
    setInterval(async () => {
        if (pending) return;
        pending = true;
        try {
            const response = await fetch("live.json", {
                credentials: "omit",
                cache: "no-store",
            });
            const payload = await response.json();
            if (
                payload.ok &&
                payload.sha256 !== applied &&
                typeof window.aurumApplyTuning === "function"
            ) {
                if (sent !== payload.sha256) {
                    window.aurumApplyTuning(
                        JSON.stringify({
                            values: payload.values,
                            sha256: payload.sha256,
                        }),
                    );
                    sent = payload.sha256;
                }
                if (window.aurumAppliedTuning === payload.sha256) {
                    applied = payload.sha256;
                    tell("aurum-preview-tuning", { sha256: applied });
                }
                if (window.aurumTuningError)
                    tell("aurum-preview-error", {
                        message: String(window.aurumTuningError).slice(0, 500),
                    });
            }
            if (
                typeof window.aurumState === "string" &&
                window.aurumState.length < 4096
            ) {
                tell("aurum-preview-state", {
                    state: JSON.parse(window.aurumState),
                });
            }
        } catch (_) {
            // The previous valid game state remains active after a transient read error.
        } finally {
            pending = false;
        }
    }, 500);
    // Godot's pinned export shell removes its status overlay when startGame resolves.
    // This also gives ordinary projects a ready signal without an Aurum state bridge.
    document.addEventListener(
        "DOMContentLoaded",
        () => {
            const status = document.getElementById("status");
            if (!status) {
                tell("aurum-preview-ready");
                return;
            }
            const observer = new MutationObserver(() => {
                if (!status.isConnected) {
                    tell("aurum-preview-ready");
                    observer.disconnect();
                }
            });
            observer.observe(status.parentNode, { childList: true });
            if (!status.isConnected) {
                tell("aurum-preview-ready");
                observer.disconnect();
            }
        },
        { once: true },
    );
})();
