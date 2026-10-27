# First steps with Implexity

The shortest checked route into this source checkout is the command-line implicit geometry workflow. It creates geometry and exports a mesh; it does not reproduce either paper's optimization campaign.

## Build the CLI

Install Rust through [rustup](https://rustup.rs/) and your platform's native linker. On Windows, the MSVC Rust toolchain requires the Visual Studio C++ Build Tools and a Windows SDK. On Linux or macOS, use the corresponding native compiler toolchain.

```sh
git clone https://github.com/SamuelSchlicht/Implexity.git
cd Implexity
cargo build --release --locked -p implexity-cli --bin implexity --no-default-features
```

The source pins Rust 1.98.1 in `rust-toolchain.toml`; the workspace declares a minimum Rust version of 1.95. rustup selects the pinned version when invoked in the checkout. For the front-page verification, the CLI was built on Windows with the already-installed Rust 1.99.1 stable toolchain using `cargo +stable` and the same package, binary, release, and feature selection. The pinned toolchain was not separately rebuilt for that verification.

`--no-default-features` omits the CLI's optional `dynamic` feature, including the feature-gated FSI registration and dynamic-results support. It retains the static geometry and provider composition used by this walkthrough. Build without that flag when those optional features are needed; that configuration was not part of the front-page smoke check.

## Generate and inspect a model

On Linux/macOS:

```sh
./target/release/implexity model examples --copy models
./target/release/implexity model show models/03_lattice_block.json
./target/release/implexity model export models/03_lattice_block.json --node lattice_only --spacing 0.35 -o lattice.stl
```

In Windows PowerShell:

```powershell
.\target\release\implexity.exe model examples --copy models
.\target\release\implexity.exe model show models/03_lattice_block.json
.\target\release\implexity.exe model export models/03_lattice_block.json --node lattice_only --spacing 0.35 -o lattice.stl
```

The copy command writes the three bundled example documents into `models`. An explicit copy makes this walkthrough independent of example-discovery paths. Use a fresh destination if you already maintain models there. The lattice example contains a solid skin and a gyroid sheet core; selecting `lattice_only` exports the confined infill. Coordinates and sample spacing are in millimetres.

The export smoke check produced a watertight STL with 31,684 triangles at 0.35 mm spacing. That verifies the authoring/export path for this example; it is not a mesh-convergence or performance benchmark. Refine the sampling for the geometric features and downstream analysis you need.

## Open the browser viewer

Run from the repository root:

```sh
./target/release/implexity serve
```

On Windows, use `.\target\release\implexity.exe serve`. Open [http://127.0.0.1:8765](http://127.0.0.1:8765) and stop the service with Ctrl+C when finished. The default host is loopback. Use `--port` if another local process occupies that port.

Use `serve --workspace ./workspace` to keep authored models and setup in a dedicated writable directory. Follow the [GUI walkthrough](gui.md) for geometry, physics, ageing, optimization, and output controls.

Without a supplied design, the service starts the **synthetic preview backend**; models authored in the workbench use the implicit geometry document. The command-line geometry JSON workflow above is separate from `--design`, which expects a design `.npz`. Consult `serve --help` for backend options.

## Explore optimization and interfaces

```sh
./target/release/implexity doctor
./target/release/implexity model --help
./target/release/implexity study --help
```

Study commands operate the real MCP bridge and accept study plans. They are not an automatic reproduction of the papers. To build the separate bridge executable:

```sh
cargo build --release --locked -p implexity-cli --bin implexity-mcp --no-default-features
```

That additional binary and a full study run were not part of the front-page smoke check. The CLI help references some documentation and publication-scope files absent from the inspected source snapshot; use the shipped command help and source implementation when following those paths.

Before interpreting a numerical result, inspect the selected model, inputs, units, constraints, convergence, and extracted geometry. The [method](method.md), [research notes](research.md), and repository [engineering disclaimer](../DISCLAIMER.md) describe the relevant context.
