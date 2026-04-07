# rs-1722

`rs-1722` is a Rust-based implementation of IEEE 1722 (Audio Video Transport Protocol, AVTP), designed for
deterministic, low-latency media streaming over Ethernet, running on Linux.

It integrates precise time synchronization (via IEEE 1588 / PTP) to enable time-aware delivery suitable for TSN and AVB
systems.

Note that this integration relies on [linuxptp (`ptp4l`)][linuxptp], which you will need to have installed before
running this program.

[linuxptp]: https://github.com/richardcochran/linuxptp/tree/master
