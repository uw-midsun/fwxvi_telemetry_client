# Setup & Running

## For end users — no build required

Download `telemetry_client.exe` and `global_can.yaml` from the [GitHub Releases](../../releases) page and place them in the same folder. Double-click the exe to run.

On first launch the app auto-creates:

```
your-folder/
├── telemetry_client.exe
├── global_can.yaml       ← must be here (or set path in Settings)
├── settings.toml         ← auto-created; stores COM port, baud, paths
└── data/
    └── decoded_data.sqlite   ← auto-created on first connection
```

No Python, Docker, or additional installs needed.

---

## For developers — building from source

### Prerequisites

1. **Rust toolchain** — [rustup.rs](https://rustup.rs)
2. **MSVC build tools** — install [Visual Studio Build Tools 2022](https://aka.ms/vs/17/release/vs_BuildTools.exe) with the **Desktop development with C++** workload

### Build

```powershell
cargo build --release
```

The output binary is at `target/release/telemetry_client.exe`. Copy it alongside `can/global_can.yaml` to distribute.

### Dev build (faster compile, no optimisations)

```powershell
cargo build
# runs from:
target/debug/telemetry_client.exe
```

---

## Updating CAN definitions

`global_can.yaml` is a generated file — never edit it by hand. To regenerate after pulling new board definitions:

```powershell
python -m scripts.tools.file_fetcher      # fetch latest board YAMLs from fwxvi repo
python -m scripts.tools.global_can_gen    # rebuild global_can.yaml
```

Then copy the new `can/global_can.yaml` next to the exe.
