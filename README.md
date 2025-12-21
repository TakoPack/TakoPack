# Takopack

Takopack is a tool for packaging software from various language ecosystems (currently supports Rust/Cargo) into RPM spec files for Linux distributions.

## Features

- **Single Package**: Generate RPM spec file for a single crate
- **Vendor Mode**: Recursively package a crate and all its dependencies
- **Cargo.toml Support**: Generate spec files directly from local Cargo.toml files
- **Dependency Resolution**: Automatically parse and generate specs for all dependencies

## Installation

```bash
cargo install --path .
```

Or build from source:

```bash
cargo build --release
```

## Usage

### Cargo Subcommands

All Rust/Cargo operations are under the `cargo` subcommand:

#### Update crates.io Index

```bash
takopack cargo update
# or use alias
takopack cargo u
```

#### Package a Single Crate

```bash
# Package a specific version
takopack cargo package <CRATE_NAME> <VERSION>

# Package latest version
takopack cargo package <CRATE_NAME>

# With custom config
takopack cargo package <CRATE_NAME> <VERSION> --config config.toml

# Examples
takopack cargo package serde 1.0.210
takopack cargo pkg tokio  # using alias
```

**Output**: Creates `rust-{crate}-{version}/rust-{crate}.spec` with only the spec file. All temporary files (source code, tar files, etc.) are automatically cleaned up.

#### Vendor Mode (Recursive Packaging)

Recursively package a crate and all its dependencies:

```bash
# Basic usage
takopack cargo vendor <CRATE_NAME> <VERSION>

# Specify output directory
takopack cargo vendor <CRATE_NAME> <VERSION> -o output_dir

# Without output dir, creates timestamped directory (e.g., 20251221_140321/)
takopack cargo vendor pyo3 0.26.0

# With output directory
takopack cargo vendor pyo3 0.26.0 -o pyo3-deps

# Using alias
takopack cargo v serde 1.0.210 -o ./deps
```

**Output**: Creates a directory (timestamped or specified) containing:
```
output_dir/
  ├── rust-crate1/
  │   └── rust-crate1.spec
  ├── rust-crate2/
  │   └── rust-crate2.spec
  └── ...
```

#### Generate Spec from Local Cargo.toml

Generate a spec file from a local Cargo.toml without downloading:

```bash
# Basic usage
takopack cargo fromtoml path/to/Cargo.toml

# Specify output directory
takopack cargo fromtoml path/to/Cargo.toml -o output_dir

# Using alias
takopack cargo from ./Cargo.toml -o specs/
```

**Output**: Creates `rust-{crate}.spec` in the current directory or specified output directory.

#### Parse Dependencies from Cargo.toml

Parse all dependencies from a Cargo.toml and recursively generate spec files:

```bash
# Basic usage (creates timestamped directory)
takopack cargo parsetoml path/to/Cargo.toml

# Specify output directory
takopack cargo parsetoml path/to/Cargo.toml -o deps_output

# Using alias
takopack cargo parse ./Cargo.toml -o ./all-deps
```

**Output**: Similar to vendor mode, creates a directory with spec files for all dependencies.

## Command Aliases

For convenience, shorter aliases are available:

- `update` → `u`
- `package` → `pkg`
- `vendor` → `v`
- `fromtoml` → `from`
- `parsetoml` → `parse`

## Examples

### Example 1: Package a Single Crate

```bash
# Package winapi 0.3.9
takopack cargo package winapi 0.3.9
```

Output structure:
```
rust-winapi-0.3.9/
└── rust-winapi.spec
```

### Example 2: Vendor All Dependencies

```bash
# Vendor pyo3 and all its dependencies
takopack cargo vendor pyo3 0.26.0 -o pyo3-vendor
```

Output structure:
```
pyo3-vendor/
├── rust-pyo3/
│   └── rust-pyo3.spec
├── rust-libc/
│   └── rust-libc.spec
├── rust-memoffset/
│   └── rust-memoffset.spec
└── ... (all dependencies)
```

### Example 3: Generate from Local Project

```bash
# Generate spec for current project
takopack cargo fromtoml ./Cargo.toml

# Parse all dependencies of current project
takopack cargo parse ./Cargo.toml -o project-deps
```

## Output Format

All generated spec files follow RPM spec format with:

- Proper `crate()` provides/requires for Rust crates
- Version constraints from Cargo dependencies
- Feature dependencies properly handled
- Automatic license and metadata extraction

## Environment Variables

- `RUST_LOG`: Set logging level (e.g., `RUST_LOG=debug takopack cargo package serde`)

## Future Support

Takopack is designed to support multiple language ecosystems:

- ✅ Rust/Cargo (currently implemented)
- 🚧 Perl/CPAN (planned)
- 🚧 Python/PyPI (planned)
- 🚧 Go modules (planned)

## License

This project is licensed under the MIT OR Apache-2.0 license.

## Contributing

Contributions are welcome! Please feel free to submit issues and pull requests.
