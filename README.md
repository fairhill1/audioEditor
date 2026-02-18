# Audio Editor

A lightweight, fast multi-track audio editor.

![screenshot](screenshots/1.png)

## Features

- Multi-track timeline with GPU-rendered waveforms
- Import MP3, WAV, FLAC, OGG, and AAC
- Non-destructive editing with full undo/redo
- Cut, copy, paste, split, and delete clips
- Drag clips between tracks with snap-to-edge
- Multi-select with Cmd+Click and Shift+Click
- Per-clip and per-track gain control
- Click track / metronome generator
- Project save/load and WAV export

## Shortcuts

| Key | Action |
|-----|--------|
| Cmd+I | Import audio file |
| Cmd+T | New track |
| Cmd+O | Open project |
| Cmd+S | Save project |
| Cmd+Shift+S | Save as |
| Cmd+E | Export WAV |
| Cmd+Z / Cmd+Shift+Z | Undo / Redo |
| Cmd+C / Cmd+X / Cmd+V | Copy / Cut / Paste |
| Cmd+Up/Down | Adjust clip gain |
| Cmd+G | Generate click track |
| Space | Play / Pause |
| S | Split clip at playhead |
| M | Toggle track mute |
| Backspace | Delete clip or selection |
| Arrow keys | Seek (Shift for fine) |

## Build & Run

Requires Rust 2024 edition (1.85+).

```
cargo run
```

## Stack

- **wgpu** — GPU rendering
- **winit** — windowing and input
- **cpal** — audio playback
- **symphonia** — audio decoding
- **glyphon** — text rendering
