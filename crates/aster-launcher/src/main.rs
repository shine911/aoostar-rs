#![forbid(non_ascii_idents)]
#![deny(unsafe_code)]

mod config;
mod logging;
mod process;
#[cfg(windows)]
mod tray;

fn main() {
    #[cfg(windows)]
    windows_main();

    #[cfg(not(windows))]
    {
        eprintln!("aster-launcher is Windows-only.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn windows_main() {
    unimplemented!("wired up in a later task")
}
