//! A network interface's own settings: up or down, its address, and the
//! default route through it, set the way ifconfig and route set them.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The kernel's `struct rtentry`, which the C library of a phone does not
/// describe.
#[repr(C)]
struct Route {
    pad1: libc::c_ulong,
    dst: libc::sockaddr,
    gateway: libc::sockaddr,
    genmask: libc::sockaddr,
    flags: libc::c_ushort,
    pad2: libc::c_short,
    pad3: libc::c_ulong,
    pad4: *mut libc::c_void,
    /// The route's metric plus one.
    metric: libc::c_short,
    dev: *mut libc::c_char,
    mtu: libc::c_ulong,
    window: libc::c_ulong,
    irtt: libc::c_ushort,
}

const RTF_UP: libc::c_ushort = 0x1;
const RTF_GATEWAY: libc::c_ushort = 0x2;

fn socket() -> Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("make a socket to set an interface with");
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn request(interface: &str) -> Result<libc::ifreq> {
    let mut r: libc::ifreq = unsafe { std::mem::zeroed() };
    anyhow::ensure!(interface.len() < r.ifr_name.len(), "{interface} is too long a name for an interface");
    for (to, from) in r.ifr_name.iter_mut().zip(interface.bytes()) {
        *to = from as libc::c_char;
    }
    Ok(r)
}

fn ioctl(fd: &OwnedFd, what: libc::c_int, arg: *mut libc::c_void, doing: &str) -> Result<()> {
    if unsafe { libc::ioctl(fd.as_raw_fd(), what, arg) } != 0 {
        return Err(std::io::Error::last_os_error()).context(doing.to_string());
    }
    Ok(())
}

fn sockaddr(address: Ipv4Addr) -> libc::sockaddr {
    let inet = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(address.octets()) },
        sin_zero: [0; 8],
    };
    unsafe { std::mem::transmute::<libc::sockaddr_in, libc::sockaddr>(inet) }
}

/// The interface's hardware address.
pub fn hardware_address(interface: &str) -> Result<[u8; 6]> {
    let fd = socket()?;
    let mut r = request(interface)?;
    ioctl(&fd, libc::SIOCGIFHWADDR as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("read the hardware address of {interface}"))?;
    let data = unsafe { r.ifr_ifru.ifru_hwaddr.sa_data };
    let mut mac = [0u8; 6];
    // A C char is signed on some phones and not on others.
    for (to, from) in mac.iter_mut().zip(data) {
        *to = from.to_ne_bytes()[0];
    }
    Ok(mac)
}

/// Bring the interface up, or take it down.
pub fn set_up(interface: &str, up: bool) -> Result<()> {
    let fd = socket()?;
    let mut r = request(interface)?;
    ioctl(&fd, libc::SIOCGIFFLAGS as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("read how {interface} stands"))?;
    unsafe {
        if up {
            r.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        } else {
            r.ifr_ifru.ifru_flags &= !(libc::IFF_UP as libc::c_short);
        }
    }
    let doing = if up { format!("bring {interface} up") } else { format!("take {interface} down") };
    // A driver still starting its interface, its firmware loading, answers
    // EBUSY: it is tried again for a while rather than the WLAN being given
    // up on the moment it was asked for.
    let mut tries = 0;
    loop {
        let done = ioctl(&fd, libc::SIOCSIFFLAGS as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &doing);
        match done {
            Ok(()) => return Ok(()),
            Err(e) if busy(&e) && tries < 60 => {
                std::thread::sleep(std::time::Duration::from_millis(250));
                tries += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Whether `e` is the kernel saying the interface is busy for the moment.
fn busy(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<std::io::Error>())
        .any(|io| io.raw_os_error() == Some(libc::EBUSY))
}

/// Give the interface `address` in a network of `mask`; what is sent to
/// any other goes through `router`.
pub fn configure(interface: &str, address: Ipv4Addr, mask: Ipv4Addr, router: Option<Ipv4Addr>) -> Result<()> {
    let fd = socket()?;
    let mut r = request(interface)?;
    r.ifr_ifru.ifru_addr = sockaddr(address);
    ioctl(&fd, libc::SIOCSIFADDR as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("give {interface} the address {address}"))?;
    r.ifr_ifru.ifru_netmask = sockaddr(mask);
    ioctl(&fd, libc::SIOCSIFNETMASK as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("give {interface} the mask {mask}"))?;
    let Some(router) = router else { return Ok(()) };
    let mut name: Vec<libc::c_char> = interface.bytes().map(|b| b as libc::c_char).chain([0]).collect();
    let mut route = Route {
        pad1: 0,
        dst: sockaddr(Ipv4Addr::UNSPECIFIED),
        gateway: sockaddr(router),
        genmask: sockaddr(Ipv4Addr::UNSPECIFIED),
        flags: RTF_UP | RTF_GATEWAY,
        pad2: 0,
        pad3: 0,
        pad4: std::ptr::null_mut(),
        metric: 1,
        dev: name.as_mut_ptr(),
        mtu: 0,
        window: 0,
        irtt: 0,
    };
    // A default route left from an earlier address goes first; the kernel
    // keeps only one.
    if unsafe { libc::ioctl(fd.as_raw_fd(), libc::SIOCDELRT as libc::c_int, (&mut route as *mut Route).cast::<libc::c_void>()) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(e).with_context(|| format!("take away the default route through {interface}"));
        }
    }
    ioctl(&fd, libc::SIOCADDRT as libc::c_int, (&mut route as *mut Route).cast(), &format!("route through {router} on {interface}"))
}

/// Take the interface's address away, and the routes through it with it.
pub fn unconfigure(interface: &str) -> Result<()> {
    let fd = socket()?;
    let mut r = request(interface)?;
    r.ifr_ifru.ifru_addr = sockaddr(Ipv4Addr::UNSPECIFIED);
    ioctl(&fd, libc::SIOCSIFADDR as libc::c_int, (&mut r as *mut libc::ifreq).cast(), &format!("take the address of {interface} away"))
}
