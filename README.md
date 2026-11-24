# Otis

A lightweight, keyboard-first parametric equalizer and by-ear headphone tuner
for PipeWire.

Otis is a tuning workspace and a PipeWire config generator, not a DSP host:

    Open Otis → listen to the sine sweep → find a problem frequency → adjust a
    band → compare with bypass → Apply to PipeWire → close Otis.

Once applied, PipeWire's own `param_eq` node does all processing. Otis does
not need to keep running.

## Requirements

- A systemd-based Linux desktop with **PipeWire 1.4 or newer** (and
  WirePlumber). Otis shows a clear error on older versions.
- To build: Rust (2024 edition), `clang`, `pkg-config`, and the PipeWire
  development headers (`libpipewire-0.3-dev` / `pipewire-devel` / `libpipewire`).

## Build and run

    cargo build --release
    ./target/release/otis

## How it works

**Apply to PipeWire** writes one self-contained drop-in,
`~/.config/pipewire/pipewire.conf.d/99-otis-eq.conf`, and restarts your active
PipeWire user services. The file creates a virtual sink, **Otis EQ**, pinned to
the hardware output you chose, so headphone EQ never spills onto speakers.
**Disable EQ** deletes the file and restarts again. Otis never reads the file
back; presets are the source of truth.

The sweep preview is a sine oscillator run through the same biquad maths as
PipeWire's `param_eq`, sent straight to the hardware output so it bypasses any
EQ that is already live. It starts at -24 dBFS and is capped at -6 dBFS.

## Tuning

- Up to **63 bands** (PipeWire's `param_eq` holds 64 biquads per channel; one
  is the preamp), 20 Hz–20 kHz, Q 0.1–18, gain ±30 dB.
- Filter types: Peak, Low Shelf, High Shelf, Low Pass, High Pass.
- **Auto preamp** (on by default) sets the preamp so the combined curve,
  including crossfeed, peaks just under 0 dBFS. Turn it off to set it by hand.
- **Crossfeed** (optional) blends a delayed, low-passed share of each channel
  into the other. Feed level ranges from -12 to -3 dB (default -6 dB) and
  delay from 0 to 500 µs (default 260 µs). It is applied in PipeWire only;
  the sweep preview does not include it.

## Keyboard

The graph is not editable with the mouse; only the sweep cursor can be dragged.
Clicking a numbered marker selects that band.

| Key | No band selected | Band selected |
|-----|------------------|---------------|
| `h` `l` / `←` `→` | Move the sweep cursor | Move the band (and cursor) |
| `j` `k` / `↓` `↑` | | Gain down / up |
| `[` `]` | | Wider / narrower (Q) |
| `a` / `Enter` | Add a band at the cursor | |
| `m` | | Toggle the band on/off |
| `d` / `Delete` | | Delete the band |
| `Esc` | Stop the sweep | Deselect |

Everywhere: `Space` sweep on/off, `b` bypass, `Tab` / `Shift+Tab` next /
previous band, `1`–`9` jump to a band, `Ctrl+Z` / `Ctrl+Shift+Z` undo / redo.

Steps are 2 % frequency, 0.5 dB and 0.1 Q. Hold `Shift` for coarse steps
(10 %, 2 dB, 0.5 Q) or `Alt` for fine ones (0.5 %, 0.1 dB, 0.02 Q). Shortcuts
are suppressed while a text or number field has focus.

## Presets and files

Presets are plain Equalizer APO text, so files from AutoEQ, Oratory1990 and
Squiglink import directly (Import…, or drop a file on the window):

    # otis: auto_preamp=on crossfeed=off feed_db=-6.0 delay_us=260
    Preamp: -4.2 dB
    Filter 1: ON LSC Fc 105 Hz Gain 3.5 dB Q 0.71
    Filter 2: ON PK Fc 6400 Hz Gain -4.0 dB Q 4.50

| Path | Purpose |
|------|---------|
| `~/.config/otis/presets/<name>.txt` | Your presets |
| `~/.config/otis/state.txt` | Last workspace, restored on launch |
| `~/.config/otis/devices.toml` | Hardware outputs (by `node.name`) and their assigned preset |
| `~/.config/otis/config.toml` | Preferences |

## Hot-swap daemon (optional)

If you switch between a DAC, a headphone jack and Bluetooth, enable the daemon
in Settings. `otis --daemon` (the `otis.service` user unit) watches for outputs
appearing and disappearing, re-targets Otis EQ, and applies the preset assigned
to each device, by rewriting the same drop-in and restarting PipeWire. It does
no audio processing, pauses while the Otis window is open, and hosts the tray
icon (`--no-tray` to disable).

A device's assigned preset is whatever you last applied for it. The daemon
only steps in when the output device changes; it leaves a device you tuned and
applied by hand alone.

## License

Otis is licensed under the GNU General Public License, version 3 or (at your option)
any later version (GPL-3.0-or-later). See [LICENSE.txt](LICENSE.txt) for the full text.
