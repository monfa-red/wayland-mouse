# wayland-mouse

**Make your mouse feel right on Wayland — pointer acceleration, scroll-wheel acceleration, and button remapping, all tuned live in a tiny terminal app.**

Wayland hands you almost no control over how your mouse *feels*: no scroll-wheel speed, only a blunt pointer-acceleration curve, and no way to remap the side buttons. `wayland-mouse` is a small, fast Rust daemon that fixes all three — it works below the compositor, so it applies in **every** app (browsers, terminals, Electron, games), and you shape it with a colorful live tuner instead of editing config files blind.

It's open source, dependency-light, and effectively free at runtime — no GUI toolkit, no idle CPU, just a ~2 MB binary and a systemd service.

## Why you'll want it

- 🖱️ **macOS-style pointer acceleration** — pixel-precise when you move slowly, smoothly faster as you flick, with a soft cap so big flicks fly without teleporting.
- 🌀 **Scroll-wheel acceleration** — slow scrolling stays 1:1 for precision; spin faster and it ramps up so you blow through long pages in one flick.
- 🎯 **Button remapping** — map side buttons to any key combo (e.g. **back/forward → switch workspace**). Sent as normal keystrokes, so it works the same on GNOME, KDE, sway, and Hyprland.
- 🎛️ **A live terminal tuner** — `wayland-mouse tune` draws each curve with a marker that **rides the curve as you move the mouse**. You tune by feel and see exactly what you're doing.
- 🎯 **DPI-independent** — calibrate once; the feel stays the same across mice and resolutions. (And if you tune by feel, you never have to think about DPI at all.)
- 🦀 **Built for fast mice** — forwards an **8000 Hz** input stream with microsecond latency and zero per-event allocation. You won't feel it's there.
- 🛡️ **Safe by design** — a small systemd service; the device grab is tied to the process, so if it ever stops, your mouse instantly returns to normal. No lockups.

## Tune it live

The heart of it is `sudo wayland-mouse tune` — a keyboard-driven terminal UI with four tabs. Tune with the keyboard while you use the mouse; every change applies instantly, and `s` saves it.

### 🖱️ Pointer — precise when slow, far-reaching when fast

<p align="center"><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/pointer.png" width="660" alt="Pointer tuning tab"></p>

A macOS-like S-curve over your cursor speed. **Precision** sets how 1:1 it is when you move slowly (lower = more accurate); **Reach** is how far fast flicks travel; **Knee speed** and **Transition width** decide *where* and *how sharply* it ramps from one to the other. The green line is gain vs. speed, and the red dot is *you* — move the mouse and watch it ride the curve, so you can dial in the exact feel by hand.

### 🌀 Wheel — gentle clicks, fast pages

<p align="center"><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/wheel.png" width="660" alt="Scroll-wheel tuning tab"></p>

The same idea for scrolling. Below **Start speed** the wheel is untouched (1:1, for precise clicks); past it, the multiplier grows by **Strength** along a **Curve**, up to a **Max multiplier** so a quick spin flies through a long document. The live readout shows your real scroll speed and the multiplier being applied right now.

### 🎯 Buttons — side buttons that do what you want

<p align="center"><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/buttons.png" width="660" alt="Button remapping tab"></p>

Press **`a`**, press the mouse button you want to map, and type a shortcut — done, applied live. The classic setup is the side back/forward buttons switching workspaces (`Super+Page_Up` / `Super+Page_Down`). Combos are sent on a virtual keyboard, so your compositor treats them like any global shortcut — no GNOME-only D-Bus, portable everywhere.

### ⚙️ General — a preset, then make it yours

<p align="center"><img src="https://raw.githubusercontent.com/monfa-red/wayland-mouse/main/docs/general.png" width="660" alt="General / presets tab"></p>

Start from a preset — **`mac-like`** (the default), **`subtle`**, or **`off`** — then tweak any knob to taste. DPI lives here too, but it's optional: if you're tuning by feel on the other tabs, you can ignore it entirely.

## Install

You need a `uinput`-capable Linux kernel (standard). Then either:

**With Rust:**

```bash
cargo install wayland-mouse
sudo ~/.cargo/bin/wayland-mouse install
```

> The full `~/.cargo/bin/` path is used because `sudo` doesn't search your Cargo bin directory. After this, the binary lives in `/usr/local/bin`, so plain `wayland-mouse …` and `sudo wayland-mouse tune` work from anywhere. (If `cargo install` warned that `~/.cargo/bin` isn't on your `PATH`, add it so the other commands resolve.)

**Without Rust** (prebuilt static binary): grab `wayland-mouse` from the [Releases](https://github.com/monfa-red/wayland-mouse/releases) page, then:

```bash
chmod +x wayland-mouse
sudo ./wayland-mouse install
```

`install` drops the binary in `/usr/local/bin`, sets up a systemd service and a `mac-like` config at `/etc/wayland-mouse/config.toml`, ensures `uinput` loads at boot, and — on GNOME — switches off GNOME's own pointer acceleration so the two curves don't fight (restored by `uninstall`).

Then run `sudo wayland-mouse tune` and make it yours.

## Commands

```
wayland-mouse run          # run the daemon (what the service runs); default subcommand
wayland-mouse install      # install binary, service, config, desktop integration
wayland-mouse uninstall    # remove all of the above, restore desktop settings
wayland-mouse status       # service state + effective config
wayland-mouse buttons      # identify your mouse buttons by name
wayland-mouse tune         # the live tuning UI
wayland-mouse config --print | --check
```

> The `tune` UI ships by default. For a daemon-only binary, build with `cargo build --no-default-features`.

## Configure by hand (optional)

Don't like terminal UIs? Everything the tuner does is plain TOML in [`/etc/wayland-mouse/config.toml`](wayland-mouse.toml.example):

```toml
preset = "mac-like"        # mac-like | subtle | off

[pointer]
max_gain = 3.0             # more reach on fast flicks

[[button]]
match = "BTN_SIDE"
keys  = ["Super", "Page_Up"]

[[device]]                 # optional per-device rules
match = "Trackball"
[device.pointer]
enabled = false            # wheel-only for this one
```

`wayland-mouse config --check` validates it; `--print` shows the resolved values.

## How it works

The daemon grabs each physical mouse via evdev and re-emits its events through a virtual `uinput` device, applying a logistic gain curve to pointer motion and a velocity-dependent multiplier to the wheel — both measured from kernel event timestamps, so they're polling-rate-independent. Motion and unmapped buttons pass through as a near-free copy, so only the curve math costs anything.

## Desktop support

The core is desktop-agnostic — **GNOME, KDE, sway, Hyprland**, any libinput Wayland compositor. The only desktop-specific bit is turning off the compositor's *own* pointer acceleration so it doesn't stack on ours: automated on GNOME, a one-line hint elsewhere. **Wheel and button features need no flip at all** and are fully portable.

> **About your existing mouse acceleration.** For pointer accel to feel right, the compositor's own acceleration has to be off — otherwise the two curves stack. On **GNOME**, `install` does this for you (sets *Acceleration Profile → Flat* and *Speed → 0*, restored on uninstall); run **`wayland-mouse status`** any time to confirm it's still `flat ✓` — it warns you if you've turned GNOME's accel back on. On **other desktops**, set your pointer acceleration profile to *flat* once (install prints the exact line). If you only use wheel/button features, none of this matters.

## License

MIT
