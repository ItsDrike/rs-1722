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

Default filtering is `info,rs_1722=debug`. Override it with `RUST_LOG`.

Configurable filters:

- global default: `info`
- application logs: `rs_1722=debug`
- `pmc` snapshot polling: `rs_1722::ptp::snapshot_poll`
- forwarded `ptp4l` process logs: `rs_1722::ptp::ptp4l` (stdout is emitted at `TRACE`, stderr at `WARN`)

Example:

```bash
RUST_LOG='info,rs_1722=trace,rs_1722::ptp::snapshot_poll=off,rs_1722::ptp::ptp4l=warn' cargo run
```
