import { defineConfig } from "@playwright/test";

export default defineConfig({
    testDir: ".",
    testMatch: "*.spec.js",
    timeout: 180_000,
    expect: { timeout: 15_000 },
    workers: 1,
    retries: 0,
    reporter: [["list"], ["html", { open: "never" }]],
    use: {
        viewport: { width: 1488, height: 1056 },
        screenshot: "only-on-failure",
        // Explicit gameplay screenshots remain mandatory. Per-action DOM
        // snapshots stall software-rendered Wasm; retain action/source traces
        // and bounded HTTP diagnostics without snapshot instrumentation.
        trace: { mode: "retain-on-failure", screenshots: false, snapshots: false, sources: true },
        launchOptions: {
            args:
                process.env.AURUM_BROWSER_HARDWARE === "1"
                    ? []
                    : [
                          "--use-angle=swiftshader",
                          "--enable-unsafe-swiftshader",
                      ],
        },
    },
});
