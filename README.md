# rs-1722

`rs-1722` is a Rust-based implementation of IEEE 1722 (Audio Video Transport Protocol, AVTP), designed for
deterministic, low-latency media streaming over Ethernet, running on Linux.

It integrates precise time synchronization (via IEEE 1588 / PTP) to enable time-aware delivery suitable for TSN and AVB
systems.

Note that this integration relies on [linuxptp (`ptp4l`)][linuxptp], which you will need to have installed before
running this program.

[linuxptp]: https://github.com/richardcochran/linuxptp/tree/master

## Logging

The project uses `tracing` with `tracing-subscriber` for structured logging.

Default filtering is set to `info,rs_1722=debug`. You can override this with the `RUST_LOG` environment variable:

```bash
RUST_LOG=debug cargo run
RUST_LOG=rs_1722=trace cargo run
```
