use crate::config::Config;
use crate::daemon;
use crate::logs;
use crate::service;
use std::io::{self, Write};

pub fn run(with_config: bool) -> io::Result<()> {
    let state = logs::state_dir();
    let config_base = Config::config_base().join("ramo");
    let config_dir_opt = Config::config_dir();

    if with_config {
        eprintln!("This will remove:");
        eprintln!("  - systemd service (if installed) at {}", service::unit_path().display());
        eprintln!("  - state at {}", state.display());
        eprintln!("  - config at {}", config_base.display());
    } else {
        eprintln!("This will remove:");
        eprintln!("  - systemd service (if installed) at {}", service::unit_path().display());
        eprintln!("  - state at {}", state.display());
        eprintln!(
            "Config at {} will be kept (use --with-config to remove it).",
            config_base.display()
        );
    }
    print!("Are you sure? [y/N] ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("aborted");
        return Ok(());
    }

    let _ = service::uninstall();
    daemon::kill();

    match std::fs::remove_dir_all(&state) {
        Ok(()) => println!("removed {}", state.display()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            println!("no state at {} (already clean)", state.display())
        }
        Err(e) => eprintln!("failed to remove {}: {e}", state.display()),
    }

    if with_config {
        let target = config_dir_opt.unwrap_or(config_base);
        match std::fs::remove_dir_all(&target) {
            Ok(()) => println!("removed {}", target.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                println!("no config at {} (already clean)", target.display())
            }
            Err(e) => eprintln!("failed to remove {}: {e}", target.display()),
        }
    } else if config_base.is_dir() {
        println!("kept config at {}", config_base.display());
    }

    if let Ok(exe) = std::env::current_exe() {
        println!();
        println!("To remove the binary itself:");
        println!("  cargo uninstall ramo  # if installed via cargo");
        println!("  rm {}  # or remove manually", exe.display());
        println!("Then restart your shell.");
    }

    println!();
    if with_config {
        println!("Ramo state and config purged.");
    } else {
        println!("Ramo state purged (config kept).");
    }
    Ok(())
}
