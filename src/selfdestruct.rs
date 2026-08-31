use std::io;

pub fn run(with_config: bool) -> io::Result<()> {
    eprintln!(
        "warning: `ramo self-destruct` is deprecated, use `ramo purge{}` instead",
        if with_config { " --with-config" } else { "" }
    );
    crate::purge::run(with_config)
}
