# wayland-mouse

Pointer acceleration, scroll-wheel acceleration, and mouse-button remapping for Wayland, configured from a terminal UI.

Wayland has no scroll-wheel speed setting, only a coarse pointer-acceleration curve, and no button remapping. `wayland-mouse` is a small daemon that grabs the mouse with evdev and re-emits it through a virtual device, so its curves apply in every application. It is written in Rust, has no GUI dependencies, and is idle when you aren't moving the mouse.

## Features

- **Pointer acceleration.** A logistic gain curve: 1:1 at low speed for precision, higher gain at high speed, with a ceiling. The `mac-like` preset approximates macOS.
- **Wheel acceleration.** Unchanged when scrolling slowly; multiplied when scrolling fast, so long pages take one flick.
- **Button remapping.** Map any button to a key combination, e.g. the side buttons to workspace switch. Emitted as keystrokes, so it is independent of the compositor.
- **Live tuning.** `wayland-mouse tune` plots each curve with a marker that follows the mouse; edits apply immediately.
- **DPI- and polling-rate-independent.** Curves are derived from kernel event timestamps. Tested at 8000 Hz.
- **Portable.** Works on GNOME, KDE, sway, Hyprland, or any libinput compositor.

## Tuning

`sudo wayland-mouse tune` opens a four-tab UI. Move between knobs with the arrow keys, adjust with left/right, switch tabs with Tab, save with `s`.

<table>
  <tr>
    <td width="50%"><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/pointer.png" alt="Pointer tab"><br><sub><b>Pointer</b>: gain vs. cursor speed; the dot is your current speed.</sub></td>
    <td width="50%"><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/wheel.png" alt="Wheel tab"><br><sub><b>Wheel</b>: multiplier vs. scroll speed.</sub></td>
  </tr>
  <tr>
    <td><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/buttons.png" alt="Buttons tab"><br><sub><b>Buttons</b>: press a button, type the combination.</sub></td>
    <td><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/general.png" alt="General tab"><br><sub><b>General</b>: preset and DPI.</sub></td>
  </tr>
</table>

## Install

Requires a `uinput`-capable kernel (standard on Linux).

```
cargo install wayland-mouse
sudo wayland-mouse install
```

Or download the static binary from the [releases page](https://github.com/monfa-red/wayland-mouse/releases) and run `sudo ./wayland-mouse install`.

`install` copies the binary to `/usr/local/bin`, registers a systemd service, writes a default config to `/etc/wayland-mouse/config.toml`, ensures `uinput` loads at boot, and on GNOME sets the pointer acceleration profile to flat. Then run `sudo wayland-mouse tune`.

> If `sudo` reports `command not found`, `~/.cargo/bin` is not on root's `PATH`; use `sudo "$(command -v wayland-mouse)" install`.

## Configuration

The config is TOML at `/etc/wayland-mouse/config.toml`; see the [annotated example](https://github.com/monfa-red/wayland-mouse/blob/main/wayland-mouse.toml.example) for every option. Choose a preset and override individual values:

```toml
preset = "mac-like"   # mac-like | subtle | off

[pointer]
max_gain = 3.0

[[button]]
match = "BTN_SIDE"
keys  = ["Super", "Page_Up"]
```

Validate with `wayland-mouse config --check`; `--print` shows the resolved values.

## Commands

```
run         run the daemon (default; used by the service)
install     install or reinstall the binary, service, and config
uninstall   remove them and restore desktop settings
status      service state, effective config, and GNOME accel state
tune        live tuning UI
buttons     print each button's evdev name as you press it
config      --print | --check
```

The `tune` UI is included by default; build with `--no-default-features` for a daemon-only binary.

## Pointer acceleration and the compositor

The compositor's own pointer acceleration must be disabled, or it stacks on top of this one. `install` does this on GNOME and `uninstall` restores it; `wayland-mouse status` reports whether it is still off. On other desktops, set the pointer acceleration profile to flat once (install prints the setting). Wheel and button features need no change and are fully portable.

## How it works

Each mouse is grabbed with evdev and re-emitted through a virtual `uinput` device with the curves applied. The grab is tied to the process, so if the daemon stops, the mouse returns to normal immediately. Pointer motion and unmapped buttons are copied through unmodified.

## License

MIT
