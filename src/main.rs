use ramo::cli::{Cli, Command, Daemon, Plugin};
use ramo::config::Config;
use ramo::picker::{self, Picker};
use ramo::terminal::Tui;
use ramo::{daemon, integration, model, purge, service, tmux};
use std::io;

fn main() -> io::Result<()> {
    let (mut config, mut feedbacks) = Config::new();
    let cli = Cli::new(&config);

    match cli.command {
        Command::Help => {
            println!("{}", cli.help());
            return Ok(());
        }
        Command::Daemon(Daemon::Start) => {
            return daemon::start_daemon(cli.overrides);
        }
        Command::Daemon(Daemon::Kill) | Command::Kill => {
            daemon::kill();
            return Ok(());
        }
        Command::Daemon(Daemon::Info) => {
            daemon::print_daemon_info();
            return Ok(());
        }
        Command::Daemon(Daemon::Logs) => {
            daemon::show_logs();
            return Ok(());
        }
        Command::Daemon(Daemon::Install) => {
            return service::install();
        }
        Command::Plugin(Plugin::Install) => {
            match integration::opencode::plugin_install() {
                Ok(()) => println!("plugin installed"),
                Err(e) => eprintln!("ramo plugin install: {e}"),
            }
            return Ok(());
        }
        Command::Daemon(Daemon::Uninstall) => {
            return service::uninstall();
        }
        Command::Purge { with_config } => {
            return purge::run(with_config);
        }
        Command::Config => {
            daemon::print_config();
            return Ok(());
        }
        Command::Unknown(ref cmd) => {
            println!("{}", cli.unknown(cmd));
            return Ok(());
        }
        _ => {}
    }

    feedbacks.extend(config.apply_overrides(&cli.overrides));

    let payload = daemon::fetch_once()
        .and_then(|bytes| serde_json::from_slice::<model::Payload>(&bytes).ok())
        .unwrap_or_else(|| daemon::preflight(&config, feedbacks));

    let mut screen = Tui::new()?;
    let mut picker = Picker::new(payload);

    picker.schedule_initial_fetch(cli.overrides);

    loop {
        screen.draw(|f| picker.render(f, &config))?;
        screen.poll_and_handle_events(&mut picker)?;

        match picker.tick() {
            picker::Signal::Close => break,
            picker::Signal::Goto(goto) => {
                if tmux::is_current_session(goto.session.clone()) {
                    if let (Some(window), Some(pane)) = (goto.window, goto.pane) {
                        tmux::select_pane(&goto.session, window, pane);
                    }
                } else {
                    tmux::goto(&goto);
                }
                if picker.quit {
                    break;
                }
            }
            _ => {}
        }
    }

    Ok(())
}
