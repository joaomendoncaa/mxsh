use crate::model::Opencode;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_DIRNAME: &str = "ramo";
const PLUGIN_ASSETS: &[(&str, &str)] = &[
    ("package.json", include_str!("assets/opencode/package.json")),
    ("server.js", include_str!("assets/opencode/server.js")),
    ("tui.js", include_str!("assets/opencode/tui.js")),
];

pub fn sessions(ignored_agents: &[String]) -> Option<Vec<Opencode>> {
    let arr = api("v2.session.list", &["--param", "limit=500"])?
        .get("data")?
        .as_array()?
        .clone();
    let active: HashSet<String> = api("v2.session.active", &[])
        .and_then(|v| {
            v.get("data")?
                .as_object()
                .map(|m| m.keys().cloned().collect())
        })
        .unwrap_or_default();
    Some(
        arr.iter()
            .filter_map(|item| parse_session(item, &active, ignored_agents))
            .collect(),
    )
}

pub fn plugin_install() -> Result<(), String> {
    let dir = plugin_config_dir();
    let pkg = dir.join(PLUGIN_DIRNAME);

    plugin_write_assets(&pkg)?;
    plugin_register(&dir, &pkg)
}

fn api(method: &str, params: &[&str]) -> Option<serde_json::Value> {
    let mut args = vec!["api", method];
    args.extend(params);
    let out = Command::new("opencode2").args(&args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn parse_session_timestamp(item: &serde_json::Value, key: &str) -> i64 {
    item.get("time")
        .and_then(|t| t.get(key))
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
}

fn parse_session_title(item: &serde_json::Value) -> String {
    match item
        .pointer("/title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
    {
        "" => format!("New session {}", parse_session_timestamp(item, "updated")),
        t => t.to_string(),
    }
}

fn parse_session(
    item: &serde_json::Value,
    active: &HashSet<String>,
    ignored_agents: &[String],
) -> Option<Opencode> {
    if item
        .pointer("/agent")
        .and_then(|a| a.as_str())
        .is_some_and(|a| ignored_agents.iter().any(|x| x == a))
    {
        return None;
    }

    let id = item.pointer("/id")?.as_str()?;
    let title = parse_session_title(item);
    let at = |key: &str| parse_session_timestamp(item, key);

    Some(Opencode {
        id: id.to_string(),
        title,
        directory: item.pointer("/location/directory")?.as_str()?.into(),
        time_updated: at("updated"),
        time_viewed: at("viewed"),
        is_running: active.contains(id),
    })
}

fn plugin_config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".config")
        });

    base.join("opencode")
}

fn plugin_read_registered(
    path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(serde_json::Map::new());
    };

    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON ({e})", path.display()))?;
    let mut obj = value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{} is not an object", path.display()))?;

    match obj.get("plugins") {
        Some(v) if !v.is_array() => Err(format!("{} has non-array 'plugins'", path.display())),
        None => {
            obj.insert("plugins".into(), serde_json::Value::Array(Vec::new()));
            Ok(obj)
        }
        _ => Ok(obj),
    }
}

fn plugin_write_assets(pkg: &Path) -> Result<(), String> {
    std::fs::create_dir_all(pkg).map_err(|e| format!("cannot create {}: {e}", pkg.display()))?;

    for (name, content) in PLUGIN_ASSETS {
        std::fs::write(pkg.join(name), content)
            .map_err(|e| format!("cannot write {}/{}: {e}", pkg.display(), name))?;
    }

    Ok(())
}

fn plugin_register(dir: &Path, pkg: &Path) -> Result<(), String> {
    let cli_path = dir.join("cli.json");
    let mut cli = plugin_read_registered(&cli_path)?;

    let spec = pkg.to_string_lossy();
    let plugins = cli["plugins"].as_array_mut().expect("validated array");
    if plugins.iter().any(|p| p.as_str() == Some(&spec)) {
        return Ok(());
    }

    plugins.push(spec.into_owned().into());
    let json = serde_json::to_string_pretty(&cli)
        .map_err(|e| format!("cannot serialize {}: {e}", cli_path.display()))?;
    std::fs::write(&cli_path, json).map_err(|e| format!("cannot write {}: {e}", cli_path.display()))
}
