use std::path::PathBuf;
use std::process::ExitCode;

pub fn run(args: &[String]) -> ExitCode {
    match execute(args) {
        Ok(value) => {
            println!("{}", serde_json::to_string(&value).unwrap());
            if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(message) => {
            println!("{}", serde_json::json!({"ok":false,"error":message}));
            ExitCode::FAILURE
        }
    }
}

fn execute(args: &[String]) -> Result<serde_json::Value, String> {
    let mut root = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut request = serde_json::json!({"op":"status"});
    let mut read_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                return Ok(
                    serde_json::json!({"usage":"aurum project [root] --request-json <JSON> | --request <file> [--read-only]","operations":aurum_studio_core::project_ops::OPERATIONS}),
                )
            }
            "--read-only" => read_only = true,
            "--request-json" | "--request" | "--root" | "--godot" => {
                let flag = &args[i];
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                match flag.as_str() {
                    "--root" => root = PathBuf::from(value),
                    "--godot" => std::env::set_var("AURUM_GODOT", value),
                    "--request" => {
                        request = serde_json::from_slice(
                            &std::fs::read(value).map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?
                    }
                    _ => request = serde_json::from_str(value).map_err(|e| e.to_string())?,
                }
            }
            "--json" => {}
            unknown if unknown.starts_with('-') => return Err(format!("unknown option {unknown}")),
            path => root = PathBuf::from(path),
        }
        i += 1;
    }
    aurum_studio_core::project_ops::execute(&root, &request, read_only)
}
