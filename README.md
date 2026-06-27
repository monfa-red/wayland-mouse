# wayland-mouse

**Mac-like mouse acceleration for Wayland — for both the pointer *and* the scroll wheel.**

Wayland/GNOME gives you no mouse-wheel speed control and only a blunt, capped pointer-acceleration curve. If you came from macOS, the cursor feels lifeless and the wheel makes you scroll forever. This is a tiny Rust daemon that fixes both — it intercepts the mouse below the compositor and reshapes how it moves and scrolls, in **every** app.

## Why you'll want it

- 🖱️ **macOS-style pointer acceleration** — pixel-precise when you move slowly, accelerates smoothly as you move faster, with a soft cap so fast flicks fly without teleporting. The feel GNOME won't give you.
- 🌀 **Scroll-wheel acceleration that respects your fingers** — slow scrolling stays exactly 1:1 for precision, but spin a little faster and it ramps up so you can blow through long pages and documents in one flick instead of grinding the wheel.
- 🎛️ **Fine-tunable to your hand** — every point on both curves is a config knob. A live `--debug` readout shows your real speeds so you can dial it in exactly.
- 🎯 **DPI-independent (like macOS)** — calibrate once, then just tell it your DPI; the feel stays identical across mice, resolutions, and machines.
- 🦀 **Rust, effectively zero overhead** — built for high-Hz gaming mice; forwards an **8000 Hz** input stream with microsecond latency, so the cursor never lags. Works at the kernel input layer, so it covers all apps — browsers, terminals, Electron, games.
- 🛡️ **Safe by design** — runs as a small systemd service; the device grab is tied to the process, so if it ever stops your mouse instantly returns to normal. No lockups.

## Install

Requires a Rust toolchain and a `uinput`-capable Linux kernel (standard).

```bash
cargo build --release
sudo bash install.sh
```

The installer drops in the binary, a systemd service, and your config, and switches off GNOME's own mouse acceleration (`accel-profile flat`) so the two curves don't fight — backing up your previous setting so `uninstall.sh` can restore it.

## Tune it

Everything is in [`scroll-accel.conf`](scroll-accel.conf) — wheel ramp, pointer curve, smoothing, and your `mouse_dpi`. Edit it, then re-apply:

```bash
sudo bash install.sh
```

Want to nail it against your actual hand speed? Watch the live numbers:

```bash
sudo systemctl stop scroll-accel
sudo /usr/local/bin/scroll-accel --debug   # scroll + move; note the speeds; Ctrl-C
sudo systemctl start scroll-accel
```

## How it works

The `scroll-accel` daemon grabs each physical wheel-mouse via evdev and re-emits its events through a virtual `uinput` device, applying:

- **Pointer:** a logistic gain curve over the smoothed cursor speed (measured from kernel event timestamps, so it's polling-rate-independent), with sub-count precision carried over.
- **Wheel:** a velocity-dependent multiplier on the high-resolution scroll stream; libinput reconstructs the discrete clicks.

Motion and buttons pass through as a near-free copy, so only the rare wheel/curve math costs anything.

## Manage

```bash
journalctl -u scroll-accel -f     # logs
sudo systemctl restart scroll-accel
sudo bash uninstall.sh            # remove + restore GNOME settings
```

## License

MIT
