//! Otis: parametric EQ and manual headphone tuner for PipeWire.
//!
//! One binary: the tuning GUI (`otis`) and the optional zero-DSP hot-swap
//! daemon (`otis --daemon`).

mod app;
mod daemon;
mod dsp;
mod graph;
mod model;
mod preset;
mod pw;
mod pwconf;
mod store;
mod system;
mod theme;

const HELP: &str = "\
Otis: parametric EQ and manual headphone tuner for PipeWire

Usage: otis [OPTIONS]

Options:
      --daemon     Run the background hot-swap daemon instead of the GUI
      --no-tray    With --daemon: do not show the system tray icon
  -V, --version    Print version
  -h, --help       Print help
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if let Some(unknown) = args.iter().find(|a| !["--daemon", "--no-tray", "-V", "--version", "-h", "--help"].contains(&a.as_str())) {
        eprintln!("otis: unknown argument '{unknown}'\n\n{HELP}");
        std::process::exit(2);
    }
    if has("-h") || has("--help") {
        print!("{HELP}");
    } else if has("-V") || has("--version") {
        println!("otis {}", env!("CARGO_PKG_VERSION"));
    } else if has("--daemon") {
        daemon::run(!has("--no-tray"));
    } else if let Err(e) = app::run() {
        eprintln!("otis: {e}");
        std::process::exit(1);
    }
}

