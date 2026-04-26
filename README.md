# downtime

The missing counterpart to the standard `uptime` utility.

## Build and Installation

This is a Rust program, so standard `cargo` instructions apply:

```shell
# Build it:
cargo build

# Run it:
cargo run

# Install it:
cargo install --path .
```

## Usage

```
Usage:
  downtime [options]

Options:
  -p, --pretty   show downtime in pretty format
  -h, --help     display this help and exit
  -s, --since    system down since
  -V, --version  output version information and exit
```
