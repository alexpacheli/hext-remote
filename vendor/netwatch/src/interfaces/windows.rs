// Patched for Hext Remote: the default route comes from the IP Helper
// (GetIpForwardTable2) instead of a WMI query over COM, which, with
// several network monitors starting at once, made COM use freed memory.
// The result is the same as the WMI query's: `Win32_IP4RouteTable.Name` of
// the route to 0.0.0.0 is its destination, "0.0.0.0", so that is what comes
// back when an IPv4 default route exists, and nothing when none does.

use n0_error::{e, stack_error};
use tracing::warn;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_TABLE2,
};
use windows::Win32::Networking::WinSock::AF_INET;

use super::DefaultRouteDetails;
pub(super) use super::netdev_impl::{get_state, home_router};

#[stack_error(derive, add_meta, std_sources, from_sources)]
#[non_exhaustive]
pub enum Error {
    #[allow(dead_code)] // not sure why we have this here?
    #[error("IO")]
    Io { source: std::io::Error },
    #[error("not route found")]
    NoRoute {},
    #[error("route table")]
    Win32 { source: windows_result::Error },
}

fn get_default_route() -> Result<DefaultRouteDetails, Error> {
    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    // SAFETY: Windows allocates the table and returns it in `table`.
    unsafe { GetIpForwardTable2(AF_INET, &mut table) }.ok()?;
    // SAFETY: on success `table` points to `NumEntries` rows, read here
    // only, and freed once below.
    let found = unsafe {
        let rows = std::slice::from_raw_parts(
            (*table).Table.as_ptr(),
            (*table).NumEntries as usize,
        );
        let found = rows
            .iter()
            .any(|row| row.DestinationPrefix.PrefixLength == 0);
        FreeMibTable(table.cast());
        found
    };
    if !found {
        return Err(e!(Error::NoRoute));
    }
    Ok(DefaultRouteDetails {
        interface_name: "0.0.0.0".to_string(),
    })
}

pub async fn default_route() -> Option<DefaultRouteDetails> {
    match get_default_route() {
        Ok(route) => Some(route),
        Err(err) => {
            warn!("failed to retrieve default route: {:#?}", err);
            None
        }
    }
}
