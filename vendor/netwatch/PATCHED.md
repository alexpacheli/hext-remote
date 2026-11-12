# netwatch 0.19.3, patched for Hext Remote

A copy of [`netwatch`](https://crates.io/crates/netwatch) 0.19.3 from
[n0-computer/net-tools](https://github.com/n0-computer/net-tools), © 2023 N0, INC., under MIT OR Apache-2.0
([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). It is used through `[patch.crates-io]` in the
workspace's `Cargo.toml`. Its sources are as published, except:

- `src/interfaces/windows.rs`: the default route comes from the IP Helper (`GetIpForwardTable2`, still on a
  blocking thread) instead of a WMI query over COM. It returns the same value the WMI query did (`"0.0.0.0"`,
  the destination, when an IPv4 default route exists). Concurrent WMI queries, one per iroh endpoint, made COM
  use freed memory.
- `Cargo.toml`: no `wmi` or `serde` on Windows; the test targets (not copied) are gone; an empty `[workspace]`,
  so its own tests run (`cargo test --manifest-path vendor/netwatch/Cargo.toml`, which CI runs on Windows).
- `src/lib.rs`, `src/interfaces.rs`: `#[allow(warnings)]` on the modules copied unchanged, since a path
  dependency's warnings are not capped like a registry crate's and CI denies warnings. `interfaces` and the
  patched `interfaces/windows.rs` are checked.

CI fails if `wmi` comes back into `Cargo.lock` (the patch only applies to 0.19.3). To drop this copy, upstream
`netwatch` must stop using WMI (0.20.0 still does): see below. See also "A patched copy of `netwatch`" in
[docs/DECISIONS.md](../../docs/DECISIONS.md).

## What to send upstream (not sent: the project owner decides)

An issue, or a PR with `src/interfaces/windows.rs` from here, on n0-computer/net-tools. Proposed text:

> **Windows: concurrent WMI default-route queries crash COM (use-after-free); use the IP Helper instead**
>
> On Windows, `interfaces::windows::get_default_route` runs a WMI query (`Win32_IP4RouteTable`), over COM, on a
> blocking thread, and every `netmon::Monitor` runs it (one per iroh endpoint). With several endpoints opening
> at once in one process, we saw hangs in calls that allocate and access violations on GitHub's Windows
> runners. Under full page heap, every run stopped on freed memory inside COM during those queries
> (`combase!OXIDEntry::DecRefCnt`, `CROIDTable::ClientDeRegisterOIDFromPingServer`, `CRIFTable`, and once in
> code of an already unloaded `wbemprox.dll`). Setting COM up beforehand (MTA, security), keeping WMI's DLLs
> loaded and keeping a WMI connection open did not stop it.
>
> The query's result is also not an interface name: `Win32_IP4RouteTable.Name` is the route's destination, so
> `default_route_interface` is always `"0.0.0.0"` on Windows.
>
> Reading the route table with `GetIpForwardTable2(AF_INET)` and looking for a prefix of length 0 returns the
> same value without COM, in microseconds, and drops the `wmi` dependency. With that change: 40 of 40 runs
> under page heap clean (before: stops in most batches). A better follow-up would return the actual interface
> (the row's `InterfaceIndex` → alias), but that changes the value `is_major_change` compares.
