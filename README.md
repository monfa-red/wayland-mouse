# wayland-mouse

**Mac-like mouse acceleration for Wayland — for both the pointer *and* the scroll wheel.**

Wayland/GNOME gives you no mouse-wheel speed control and only a blunt, capped pointer-acceleration curve. If you came from macOS, the cursor feels lifeless and the wheel makes you scroll forever. This is a tiny Rust daemon that fixes both — it intercepts the mouse below the compositor and reshapes how it moves and scrolls, in **every** app.

## Why you'll want it

- 🖱️ **macOS-style pointer acceleration** — pixel-precise when you move slowly, accelerates smoothly as you move faster, with a soft cap so fast flicks fly without teleporting. The feel GNOME won't give you.
- 🌀 **Scroll-wheel acceleration that respects your fingers** — slow scrolling stays exactly 1:1 for precision, but spin a little faster and it ramps up so you can blow through long pages in one flick instead of grinding the wheel.
- 🎛️ **Presets, then fine-tunable** — start from `mac-like`, `subtle`, or `off`, then override any knob. A live `--debug` readout shows your real speeds so you can dial it in.
- 🎯 **DPI-independent (like macOS)** — calibrate once, then just tell it your DPI; the feel stays identical across mice, resolutions, and machines.
- 🦀 **Rust, effectively zero overhead** — built for high-Hz gaming mice; forwards an **8000 Hz** input stream with microsecond latency. Works at the kernel input layer, so it covers all apps — browsers, terminals, Electron, games.
- 🛡️ **Safe by design** — a small systemd service; the device grab is tied to the process, so if it ever stops your mouse instantly returns to normal. No lockups.

## Install

You need a `uinput`-capable Linux kernel (standard). Then either:

**With Rust (`cargo`):**

```bash
cargo install wayland-mouse
sudo wayland-mouse install
```

**Without Rust (prebuilt static binary):** download the latest `wayland-mouse` from the [Releases](https://github.com/monfa-red/wayland-mouse/releases) page, then:

```bash
chmod +x wayland-mouse
sudo ./wayland-mouse install
```

`install` drops the binary in `/usr/local/bin`, writes a systemd service and a default config at `/etc/wayland-mouse/config.toml`, ensures `uinput` loads at boot, and — on GNOME — switches off GNOME's own mouse acceleration so the two curves don't fight (backing up your previous setting so `uninstall` restores it). It also **migrates an existing `scroll-accel` (v0.1) install** automatically.

## Configure

Everything lives in [`/etc/wayland-mouse/config.toml`](wayland-mouse.toml.example). Pick a preset and override what you like:

```toml
preset = "mac-like"     # mac-like | subtle | off
dpi = 1400              # your mouse's hardware DPI (the curve auto-rescales)

[pointer]
max_gain = 3.0          # more reach on fast flicks

[[device]]              # optional per-device rules
match = "Trackball"
[device.pointer]
enabled = false         # wheel accel only for this one
```

Apply changes and inspect the result:

```bash
sudo systemctl restart wayland-mouse
wayland-mouse config --print      # effective (resolved, DPI-rescaled) values
wayland-mouse config --check      # validate syntax, keys, and ranges
```

Tune against your actual hand speed by watching the live numbers:

```bash
sudo systemctl stop wayland-mouse
sudo wayland-mouse run --debug    # move + scroll, note the speeds, Ctrl-C
sudo systemctl start wayland-mouse
```

## Commands

```
wayland-mouse run          # run the daemon (what the service runs); default subcommand
wayland-mouse install      # install binary, service, config, desktop integration
wayland-mouse uninstall    # remove all of the above, restore desktop settings
wayland-mouse status       # service state + effective config
wayland-mouse config --print | --check
```

Global flags: `--debug` (live speed readout), `--config <path>` (use a different config file).

## How it works

The daemon grabs each physical wheel-mouse via evdev and re-emits its events through a virtual `uinput` device, applying:

- **Pointer:** a logistic gain curve over the smoothed cursor speed (measured from kernel event timestamps, so it's polling-rate-independent), with sub-count precision carried over.
- **Wheel:** a velocity-dependent multiplier on the high-resolution scroll stream; libinput reconstructs the discrete clicks.

Motion and buttons pass through as a near-free copy, so only the rare wheel/curve math costs anything.

## Desktop support

The evdev core is desktop-agnostic — it works on **GNOME, KDE, sway, Hyprland**, any libinput Wayland compositor. The only desktop-specific piece is disabling the compositor's *own* pointer acceleration so it doesn't stack on ours:

- **GNOME** — handled automatically by `install` (and restored by `uninstall`).
- **Other desktops** — `install` prints the one line to add to your settings/compositor config.
- **Wheel-only is fully portable** — set `pointer.enabled = false` and no flip is needed anywhere.

## Manage

```bash
journalctl -u wayland-mouse -f     # logs
sudo systemctl restart wayland-mouse
sudo wayland-mouse uninstall       # remove + restore GNOME settings
```

## License

MIT
