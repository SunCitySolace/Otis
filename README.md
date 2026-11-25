<p align="center">
  <img src="assets/otis.svg" width="128" alt="Otis icon">
</p>

<h1 align="center">Otis</h1>

<p align="center">
  A by-ear headphone EQ that writes a PipeWire config file, then gets out of the way.
</p>

---

Otis is a **dotfile generator**, not a DSP. You tune by ear, press **Apply**,
and Otis writes a single file:

    ~/.config/pipewire/pipewire.conf.d/99-otis-eq.conf

From then on PipeWire's built-in `param_eq` does all the processing. Close
Otis; your EQ stays. Press **Disable EQ** (or delete the file and restart
PipeWire) and it's gone.

## Workflow

1. Play the sine sweep and listen for peaks or dips.
2. Add a band at the problem frequency and adjust it.
3. Compare against bypass.
4. **Apply to PipeWire**, then close Otis.

The generated file defines a virtual sink, **Otis EQ**, pinned to the output
you chose, so headphone EQ never leaks onto your speakers.

## Requirements

- Linux with systemd, **PipeWire 1.4+** and WirePlumber
- To build: Rust (2024 edition), `clang`, `pkg-config`, PipeWire dev headers

## Build

    cargo build --release
    ./target/release/otis

## Features

- Up to 63 bands: peak, shelf, low/high pass
- Auto preamp to keep the curve below 0 dBFS
- Optional crossfeed
- Keyboard-first: `h` `j` `k` `l` to move and adjust, `a` to add a band,
  `Space` to sweep, `b` to bypass
- Presets are plain Equalizer APO text, so AutoEQ and Squiglink files import
  directly
- Optional `otis --daemon` that rewrites the config when you switch output
  devices (it never touches audio)

## Files

| Path | Purpose |
|------|---------|
| `~/.config/pipewire/pipewire.conf.d/99-otis-eq.conf` | The generated EQ |
| `~/.config/otis/presets/` | Your presets |
| `~/.config/otis/` | Otis's own state and settings |

## License

[GPL-3.0-or-later](LICENSE.txt)
