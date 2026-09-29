//! Compare native declarations while ignoring comments and function bodies.

fn tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            let mut depth = 1;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let start = i;
        if chars[i] == '"' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i = (i + 2).min(chars.len());
                } else if chars[i] == '"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if chars[i].is_alphanumeric() || chars[i] == '_' {
            i += 1;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
        } else {
            i += 1;
        }
        out.push(chars[start..i].iter().collect());
    }
    out
}

pub fn surface(text: &str) -> Vec<String> {
    let input = tokens(text);
    if !input.iter().any(|token| {
        token == "GodotClass" || token == "gdextension" || token == "godot_api" || token == "func"
    }) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if input[i] != "fn" {
            out.push(input[i].clone());
            i += 1;
            continue;
        }
        while i < input.len() && input[i] != "{" && input[i] != ";" {
            out.push(input[i].clone());
            i += 1;
        }
        if i < input.len() && input[i] == "{" {
            out.push("{}".into());
            let mut depth = 1;
            i += 1;
            while i < input.len() && depth > 0 {
                match input[i].as_str() {
                    "{" => depth += 1,
                    "}" => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
        }
    }
    out
}

pub fn classify(
    path: &std::path::Path,
    before: Option<&str>,
    after: Option<&str>,
) -> crate::reload::Classification {
    let mut result = crate::reload::classify_path(path);
    if path.extension().is_some_and(|e| e == "rs")
        && surface(before.unwrap_or_default()) != surface(after.unwrap_or_default())
    {
        result.verdict = crate::reload::Verdict::EditorRestart;
        result.reason =
            "native declarations changed; save work before restarting the editor".into();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn body_edits_reload_but_signature_changes_restart() {
        let before = "#[func] fn score(&self) -> i32 { 1 }";
        let body = "// explanation\n#[func] fn score(&self) -> i32 { let x = 2; x }";
        let signature = "#[func] fn score(&self, bonus: i32) -> i32 { bonus }";
        let path = std::path::Path::new("lib.rs");
        assert_eq!(
            classify(path, Some(before), Some(body)).verdict,
            crate::Verdict::Reload
        );
        assert_eq!(
            classify(path, Some(before), Some(signature)).verdict,
            crate::Verdict::EditorRestart
        );
        assert_eq!(
            classify(path, Some(before), None).verdict,
            crate::Verdict::EditorRestart
        );
        assert_eq!(
            classify(path, Some("fn helper() {}"), None).verdict,
            crate::Verdict::Reload
        );
    }
}
