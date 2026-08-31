use crate::config::Config;

#[derive(Debug, Clone, PartialEq)]
pub enum Daemon {
    Info,
    Start,
    Kill,
    Logs,
    Install,
    Uninstall,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run,
    Daemon(Daemon),
    Kill,
    Help,
    Config,
    Purge { with_config: bool },
    SelfDestruct { with_config: bool },
    Unknown(String),
}

pub struct Cli {
    pub command: Command,
    pub overrides: Vec<(String, Option<String>)>,
}

impl Cli {
    pub fn new(_config: &Config) -> Self {
        let args: Vec<String> = std::env::args().collect();
        let mut command = Command::Run;
        let mut overrides = Vec::new();
        let mut i = 1;

        while i < args.len() {
            match args[i].as_str() {
                "help" | "--help" | "-h" => command = Command::Help,
                "daemon" => {
                    i += 1;
                    command = if i < args.len() {
                        match args[i].as_str() {
                            "start" => Command::Daemon(Daemon::Start),
                            "kill" => Command::Daemon(Daemon::Kill),
                            "logs" => Command::Daemon(Daemon::Logs),
                            "install" => Command::Daemon(Daemon::Install),
                            "uninstall" => Command::Daemon(Daemon::Uninstall),
                            _ => Command::Unknown(unknown_cmd(&args)),
                        }
                    } else {
                        Command::Daemon(Daemon::Info)
                    };
                }
                "kill" => command = Command::Kill,
                "purge" => command = Command::Purge { with_config: false },
                "self-destruct" => command = Command::SelfDestruct { with_config: true },
                "config" => command = Command::Config,
                a if a.starts_with("--") => {
                    let rest = &a[2..];
                    let is_purge = matches!(
                        command,
                        Command::Purge { .. } | Command::SelfDestruct { .. }
                    );
                    if rest == "with-config" || rest.starts_with("with-config=") {
                        match &mut command {
                            Command::Purge { with_config } => {
                                *with_config = true;
                            }
                            Command::SelfDestruct { with_config } => {
                                *with_config = true;
                            }
                            _ => {
                                // not in purge context -> treat as normal flag override
                                // (will surface as "isn't a valid flag" later)
                                if let Some((key, value)) = rest.split_once('=') {
                                    overrides.push((key.to_string(), Some(value.to_string())));
                                } else {
                                    overrides.push((rest.to_string(), None));
                                }
                            }
                        }
                    } else if is_purge {
                        // unknown flag for purge
                        command = Command::Unknown(unknown_cmd(&args));
                    } else if let Some((key, value)) = rest.split_once('=') {
                        overrides.push((key.to_string(), Some(value.to_string())));
                    } else {
                        overrides.push((rest.to_string(), None));
                    }
                }
                _ => command = Command::Unknown(unknown_cmd(&args)),
            }
            i += 1;
        }

        Cli { command, overrides }
    }

    pub fn unknown(&self, cmd: &str) -> String {
        format!(
            "
command `{}` doesn't exist

use `ramo help` to see usage
        ",
            cmd
        )
    }

    pub fn help(&self) -> String {
        "\n\
\n\
ramo                Open picker (starts daemon if not running yet)\n\
\n\
ramo daemon         Print current daemon info\n\
ramo daemon start   Start a daemon\n\
ramo daemon kill    Kill running daemon\n\
ramo daemon logs    Tail daemon logs\n\
ramo daemon install     Install systemd user service (daemon starts at login)\n\
ramo daemon uninstall   Remove the systemd user service\n\
\n\
ramo purge                Remove daemon service and state (keeps config)\n\
ramo purge --with-config  Remove daemon service, state and config\n\
\n\
ramo kill           Alias for daemon kill\n\
\n\
ramo config         Print config path and contents\n\
\n\
ramo help           Print this help\n\
\n\
Flags override config keys (e.g. --path=/custom/path)"
            .to_string()
    }
}

fn unknown_cmd(args: &[String]) -> String {
    let prog = std::path::Path::new(&args[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ramo");
    let rest = &args[1..];
    if rest.is_empty() {
        prog.to_string()
    } else {
        format!("{} {}", prog, rest.join(" "))
    }
}
