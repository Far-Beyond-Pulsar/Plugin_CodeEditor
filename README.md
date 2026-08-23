# Plugin_CodeEditor
Adds an integrated script editor to Pulsar Engine!

<img width="3437" height="1439" alt="image" src="https://github.com/user-attachments/assets/1dad12df-6962-4bb2-83f1-2130829d793d" />
<img width="2155" height="1109" alt="image" src="https://github.com/user-attachments/assets/120873f3-5d10-4610-8557-de9b85a680ea" />
<img width="2515" height="1362" alt="image" src="https://github.com/user-attachments/assets/b1067474-92d1-4faa-9e9a-5f420032c1af" />
<img width="2515" height="1362" alt="image" src="https://github.com/user-attachments/assets/c374fe71-500e-40c8-b1d9-6e33f4173ead" />
<img width="2515" height="1362" alt="image" src="https://github.com/user-attachments/assets/d1e00d25-0d10-4aae-b39a-2468d7d1acb4" />

## Development
The standalone test harness lets you iterate on the editor without installing it into Pulsar Engine.

```sh
cargo run --example standalone -- <path-to-project> [files...]
```

This opens a maximized native window hosting the full Script Editor (file explorer + tabs + rust-analyzer LSP if `rust-analyzer` is on your PATH). If no path is given, it defaults to the current directory.

Flags:
- `--fullscreen` — run in borderless fullscreen.
- `RUST_LOG=debug cargo run ...` — enable verbose tracing output.

Assets (icons + JetBrains Mono font) are embedded from the `ui` crate — nothing to install.
