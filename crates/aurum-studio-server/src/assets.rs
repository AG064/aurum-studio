//! The page, compiled into the binary.
//!
//! The shell ships as one executable with no installation step, so the assets
//! a browser needs are embedded with `include_str!` rather than read from disk.
//! That also means the files cannot be swapped out from under a running Studio,
//! and there is no asset path to resolve — or to escape from.
//!
//! These are deliberately plain: no build step, no framework, no package
//! manager. A page this size does not need one, and every tool in that chain
//! would be another thing to keep current.

/// The page.
pub const INDEX_HTML: &str = include_str!("../ui/index.html");

/// The page's behaviour.
pub const APP_JS: &str = include_str!("../ui/app.js");
pub const WORKSPACE_JS: &str = include_str!("../ui/workspace.js");
pub const WORKSPACE_CSS: &str = include_str!("../ui/workspace.css");

/// The page's appearance.
pub const STYLE_CSS: &str = include_str!("../ui/style.css");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_references_only_assets_this_server_serves() {
        for asset in ["/app.js", "/style.css"] {
            assert!(INDEX_HTML.contains(asset), "the page should load {asset}");
        }
    }

    #[test]
    fn the_page_does_not_contain_a_token_placeholder() {
        // The server refuses to bake the token into the page, and the page is
        // expected to read it from the URL instead. A placeholder here would
        // be a step towards leaking it into history or a cache.
        for suspicious in ["{{", "__TOKEN__", "token = '", "token = \""] {
            assert!(
                !INDEX_HTML.contains(suspicious),
                "the page should not template a token: found '{suspicious}'"
            );
        }
    }

    #[test]
    fn the_script_strips_the_token_from_the_address_bar() {
        // Without this the token stays in history and can leak in a Referer.
        assert!(APP_JS.contains("replaceState"));
        assert!(APP_JS.contains("sessionStorage"));
    }

    #[test]
    fn the_script_sends_the_token_as_a_header() {
        assert!(
            APP_JS.contains("X-Aurum-Token"),
            "requests should carry the token in a header, not only the URL"
        );
    }

    #[test]
    fn the_page_has_no_external_references() {
        // Loading anything from a CDN would make the shell depend on the
        // network, and would tell a third party when it is open.
        for marker in ["http://", "https://", "//cdn", "integrity="] {
            assert!(
                !INDEX_HTML.contains(marker),
                "the page should be self-contained: found '{marker}'"
            );
            assert!(
                !STYLE_CSS.contains(marker),
                "the stylesheet should be self-contained: found '{marker}'"
            );
        }
    }

    #[test]
    fn the_assets_are_not_empty() {
        assert!(INDEX_HTML.len() > 200);
        assert!(APP_JS.len() > 500);
        assert!(STYLE_CSS.len() > 200);
    }

    #[test]
    fn the_stylesheet_styles_every_health_label_that_reaches_the_page() {
        // A level reaches the page as `Health::label()`, so the class the page
        // builds and the class the stylesheet defines must be the same string.
        // They were not once, and only running it said so. Every surface that
        // shows a level is held to it: the verdict, and each finding row.
        use aurum_studio_core::doctor::Health;
        for health in [Health::Healthy, Health::Warning, Health::Blocked] {
            for surface in ["verdict", "finding"] {
                let selector = format!(".{surface}.{}", health.label());
                assert!(
                    STYLE_CSS.contains(&selector),
                    "the stylesheet should define '{selector}'"
                );
            }
        }
    }

    #[test]
    fn the_script_renders_the_findings_behind_a_verdict() {
        // The panel was one word and a count until the findings were carried
        // through; a verdict with nothing behind it is not a diagnosis.
        for marker in ["event.findings", "finding.health", "finding.remedy"] {
            assert!(
                APP_JS.contains(marker),
                "the script should read '{marker}' rather than only the verdict"
            );
        }
    }

    #[test]
    fn the_stylesheet_styles_every_log_severity_the_script_sets() {
        // Same failure mode as the verdict: a severity the script applies and
        // the stylesheet never defines is silently unstyled, which looks like
        // a design decision rather than a bug.
        for kind in ["good", "bad", "note"] {
            assert!(
                APP_JS.contains(&format!("'{kind}'")),
                "the script should use the '{kind}' severity"
            );
            assert!(
                STYLE_CSS.contains(&format!(".log .{kind}")),
                "the stylesheet should define '.log .{kind}'"
            );
        }
    }

    #[test]
    fn every_button_asks_for_a_command_the_server_accepts() {
        // A button naming a command the server does not know is a dead control
        // that only fails when somebody presses it.
        const ACCEPTED: [&str; 8] = [
            "doctor",
            "build",
            "start-editor",
            "start-game",
            "stop",
            "develop",
            "end-develop",
            "restart-editor",
        ];

        let mut seen = 0;
        for piece in INDEX_HTML.split("data-command=\"").skip(1) {
            let command = piece.split('"').next().unwrap_or_default();
            assert!(
                ACCEPTED.contains(&command),
                "the page asks for '{command}', which the server does not accept"
            );
            seen += 1;
        }
        assert!(
            seen >= 6,
            "the page should offer the whole command set, found {seen}"
        );
    }

    #[test]
    fn the_mark_is_inline_rather_than_a_fetched_image() {
        // Inline SVG keeps the page self-contained. An `<img>` would be a
        // request, and an SVG written with an xmlns would put an external
        // reference in the source even when nothing is fetched.
        assert!(INDEX_HTML.contains("<svg"), "the mark should be inline SVG");
        assert!(
            !INDEX_HTML.contains("<img"),
            "the page should not fetch an image"
        );
        assert!(
            !INDEX_HTML.contains("xmlns="),
            "inline SVG needs no namespace declaration in HTML5, and adding \
             one puts an external-looking reference in the page"
        );
    }
}
