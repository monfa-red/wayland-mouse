//! wayland-mouse — Mac-like mouse acceleration for Wayland/libinput: a
//! velocity-dependent scroll-wheel multiplier and a macOS-like pointer S-curve,
//! applied below the compositor by grabbing each wheel mouse via evdev and
//! re-emitting through a virtual uinput device.
//!
//! Wayland/GNOME exposes no wheel-speed setting and only a blunt, capped pointer
//! curve, so the only way to get full control is to intercept the mouse. These
//! mice poll at up to 8000 Hz, so forwarding must be cheap: motion is
//! accumulated per frame and accelerated at SYN_REPORT using the kernel event
//! timestamps. The grab is fd-tied, so a crash releases the mouse instantly.
//!
//! It can also remap mouse buttons to key combos (e.g. side buttons → workspace
//! switch) via a shared virtual keyboard.
//!
//! Subcommands: `run` (default), `install`, `uninstall`, `status`, `buttons`,
//! `config`.

mod cli;
mod config;
mod desktop;
mod device;
mod install;
mod pointer;
mod remap;
mod util;
mod wheel;

fn main() {
    cli::main();
}
