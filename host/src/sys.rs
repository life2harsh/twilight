use tokio::net::TcpStream;

#[cfg(target_os = "linux")]
pub fn unsent_bytes(stream: &TcpStream) -> usize {
    use std::os::fd::AsRawFd;
    let mut value: libc::c_int = 0;
    let rc = unsafe { libc::ioctl(stream.as_raw_fd(), libc::TIOCOUTQ, &mut value) };
    if rc == 0 { value.max(0) as usize } else { 0 }
}

#[cfg(windows)]
pub fn unsent_bytes(stream: &TcpStream) -> usize {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SIO_TCP_INFO, SOCKET_ERROR, TCP_INFO_v0, WSAIoctl};
    let version: u32 = 0;
    let mut info: TCP_INFO_v0 = unsafe { std::mem::zeroed() };
    let mut returned = 0;
    let rc = unsafe {
        WSAIoctl(
            stream.as_raw_socket() as _,
            SIO_TCP_INFO,
            &version as *const u32 as _,
            size_of::<u32>() as u32,
            &mut info as *mut TCP_INFO_v0 as _,
            size_of::<TCP_INFO_v0>() as u32,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    if rc == SOCKET_ERROR { 0 } else { info.BytesInFlight as usize }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn unsent_bytes(_: &TcpStream) -> usize {
    0
}
