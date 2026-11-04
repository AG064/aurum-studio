//! GUI-only opt-in Chromium browser policy. Parsing has no environment or filesystem effects.

use std::ffi::OsStr;
#[cfg(any(windows, test))]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) const EXECUTABLE_VARIABLE: &str = "AURUM_STUDIO_BROWSER_EXECUTABLE";
pub(crate) const PROFILE_VARIABLE: &str = "AURUM_STUDIO_BROWSER_PROFILE";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConfigurationError {
    IncompletePair,
    InvalidPath,
    ExecutableExtension,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct BrowserChoice {
    executable: PathBuf,
    profile: PathBuf,
}

impl BrowserChoice {
    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn profile(&self) -> &Path {
        &self.profile
    }

    #[cfg(any(windows, test))]
    pub(crate) fn arguments(&self, url: &str) -> [OsString; 5] {
        let mut profile = OsString::from("--user-data-dir=");
        profile.push(&self.profile);
        [
            profile,
            OsString::from("--no-first-run"),
            OsString::from("--no-default-browser-check"),
            OsString::from("--new-window"),
            OsString::from(url),
        ]
    }
}

pub(crate) fn parse(
    executable: Option<&OsStr>,
    profile: Option<&OsStr>,
) -> Result<Option<BrowserChoice>, ConfigurationError> {
    let (executable, profile) = match (executable, profile) {
        (None, None) => return Ok(None),
        (Some(executable), Some(profile)) if !executable.is_empty() && !profile.is_empty() => {
            (executable, profile)
        }
        _ => return Err(ConfigurationError::IncompletePair),
    };
    let executable = local_path(executable)?;
    let profile = local_path(profile)?;
    if !executable
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(ConfigurationError::ExecutableExtension);
    }
    Ok(Some(BrowserChoice {
        executable,
        profile,
    }))
}

fn local_path(value: &OsStr) -> Result<PathBuf, ConfigurationError> {
    let value = value.to_str().ok_or(ConfigurationError::InvalidPath)?;
    let bytes = value.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
        || value.chars().any(char::is_control)
    {
        return Err(ConfigurationError::InvalidPath);
    }
    for component in value[3..].split(['\\', '/']) {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.ends_with(['.', ' '])
            || component.contains([':', '"', '<', '>', '|', '?', '*'])
            || device_component(component)
        {
            return Err(ConfigurationError::InvalidPath);
        }
    }
    Ok(PathBuf::from(value.replace('/', "\\")))
}

fn device_component(component: &str) -> bool {
    let name = component
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        name.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    ["COM", "LPT"].iter().any(|prefix| {
        name.strip_prefix(*prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "C:/Program Files/Chromium/chrome.exe";
    const PROFILE: &str = "A:/QA profile ü/chromium";

    fn configured() -> BrowserChoice {
        parse(Some(OsStr::new(EXE)), Some(OsStr::new(PROFILE)))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn absent_settings_preserve_the_system_default() {
        assert_eq!(parse(None, None), Ok(None));
    }

    #[test]
    fn incomplete_or_explicitly_empty_pairs_never_fall_back() {
        for (executable, profile) in [
            (Some(EXE), None),
            (None, Some(PROFILE)),
            (Some(""), Some("")),
            (Some(""), Some(PROFILE)),
            (Some(EXE), Some("")),
        ] {
            assert_eq!(
                parse(executable.map(OsStr::new), profile.map(OsStr::new)),
                Err(ConfigurationError::IncompletePair)
            );
        }
    }

    #[test]
    fn absolute_paths_preserve_spaces_and_unicode() {
        let choice = configured();
        assert_eq!(
            choice.executable(),
            Path::new(r"C:\Program Files\Chromium\chrome.exe")
        );
        assert_eq!(choice.profile(), Path::new(r"A:\QA profile ü\chromium"));
        assert!(parse(
            Some(OsStr::new("C:/Browser/CHROME.EXE")),
            Some(OsStr::new(PROFILE))
        )
        .is_ok());
    }

    #[test]
    fn unsafe_windows_path_forms_are_refused_for_either_setting() {
        for path in [
            "relative.exe",
            r"C:relative.exe",
            r"\rooted.exe",
            r"\\server\share\browser.exe",
            r"\\?\C:\browser.exe",
            r"\\.\pipe\browser.exe",
            r"C:\..\browser.exe",
            r"C:\.\browser.exe",
            r"C:\folder\\browser.exe",
            r"C:\browser.exe:stream",
            r"C:\folder.\browser.exe",
            r"C:\folder \browser.exe",
            r"C:\NUL.exe",
            r"C:\AUX\browser.exe",
            r"C:\COM1.exe",
            r"C:\LPT³\browser.exe",
            "C:/bad\n/browser.exe",
            "C:/bad\0/browser.exe",
            "C:/\"browser.exe\"",
            "C:/browser*.exe",
            "C:/",
        ] {
            assert!(parse(Some(OsStr::new(path)), Some(OsStr::new(PROFILE))).is_err());
            assert!(parse(Some(OsStr::new(EXE)), Some(OsStr::new(path))).is_err());
        }
    }

    #[test]
    fn executable_must_have_an_exe_extension() {
        for path in [
            "C:/Browser/chrome",
            "C:/Browser/chrome.cmd",
            "C:/Browser/chrome.exe.bat",
        ] {
            assert_eq!(
                parse(Some(OsStr::new(path)), Some(OsStr::new(PROFILE))),
                Err(ConfigurationError::ExecutableExtension)
            );
        }
    }

    #[test]
    fn arguments_are_fixed_and_never_split_profile_or_url_text() {
        let choice = parse(
            Some(OsStr::new(EXE)),
            Some(OsStr::new("A:/QA & profile/chromium --flag=value")),
        )
        .unwrap()
        .unwrap();
        let url = "http://127.0.0.1:1234/?t=synthetic&part=one";
        assert_eq!(
            choice.arguments(url),
            [
                OsString::from(r"--user-data-dir=A:\QA & profile\chromium --flag=value"),
                OsString::from("--no-first-run"),
                OsString::from("--no-default-browser-check"),
                OsString::from("--new-window"),
                OsString::from(url),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn malformed_utf16_is_refused() {
        use std::os::windows::ffi::OsStringExt;
        let malformed = OsString::from_wide(&[0xd800]);
        assert_eq!(
            parse(Some(&malformed), Some(OsStr::new(PROFILE))),
            Err(ConfigurationError::InvalidPath)
        );
    }

    #[cfg(unix)]
    #[test]
    fn malformed_utf8_is_refused() {
        use std::os::unix::ffi::OsStringExt;
        let malformed = OsString::from_vec(vec![0xff]);
        assert_eq!(
            parse(Some(&malformed), Some(OsStr::new(PROFILE))),
            Err(ConfigurationError::InvalidPath)
        );
    }
}
