# netwatch 0.19.3, patched for Hext Remote

A copy of [`netwatch`](https://crates.io/crates/netwatch) 0.19.3 (n0-computer/net-tools, MIT OR Apache-2.0),
used through `[patch.crates-io]` in the workspace's `Cargo.toml`. Its sources are as published, except:

- `src/interfaces/windows.rs`: the default route comes from the IP Helper (`GetIpForwardTable2`) instead of a
  WMI query over COM. It returns the same value the WMI query did (`"0.0.0.0"`, the destination, when an IPv4
  default route exists). Concurrent WMI queries, one per iroh endpoint, made COM use freed memory.
- `Cargo.toml`: no `wmi` or `serde` on Windows; the test targets, not copied, are gone.
- `src/lib.rs`: `#![allow(warnings)]`, since a path dependency's warnings are not capped like a registry
  crate's and CI denies warnings.

See "A patched copy of `netwatch`" in [docs/DECISIONS.md](../../docs/DECISIONS.md).
