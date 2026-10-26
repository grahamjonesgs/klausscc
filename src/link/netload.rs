//! Network program loader — stream a flat DDR image to the board over TCP.
//!
//! The board runs `netboot.c` (a resident lwIP program) listening on a TCP
//! port.  We connect, send a 12-byte header (`magic`, `img_len`, `entry_pc`,
//! all little-endian) followed by the raw DDR image bytes, then read an 8-byte
//! acknowledgement (`status`, `checksum`).  TCP itself provides the ordering /
//! loss / retransmit that the kbt-over-UART path never needed and the earlier
//! hand-rolled UDP design would have had to implement.

use crate::diag::Diagnostics;
use crate::num::human_bytes;
use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs as _};
use std::time::Duration;

/// Protocol magic — `b"KNET"` read as a little-endian u32 (matches the board's
/// `NETBOOT_MAGIC`).
const NETBOOT_MAGIC: u32 = 0x5445_4E4B;

/// Default TCP port the board's netboot server listens on.
pub(crate) const NETBOOT_DEFAULT_PORT: u16 = 5000;

/// How long to wait for the board to accept the TCP connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a single read or write may stall before the load is abandoned.
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a network load failed.
#[derive(Debug)]
pub(crate) enum NetbootError {
    /// Resolving, connecting or talking to the board failed.
    Io(io::Error),
    /// The board answered with a non-zero status.
    Rejected(u32),
    /// The board's checksum of the received image differs from ours.
    Checksum {
        /// Checksum computed on the host.
        host: u32,
        /// Checksum the board reported.
        board: u32,
    },
    /// The image is too large for the protocol's 32-bit length field.
    ImageTooLarge(usize),
}

impl fmt::Display for NetbootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Rejected(status) => write!(f, "board rejected image (status {status})"),
            Self::Checksum { host, board } => write!(f, "checksum mismatch: host 0x{host:08X}, board 0x{board:08X}"),
            Self::ImageTooLarge(len) => write!(f, "image of {len} bytes is too large for netboot (limit 4 GiB)"),
        }
    }
}

impl std::error::Error for NetbootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for NetbootError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// 32-bit additive checksum over little-endian 32-bit words.
///
/// Must match the board's `image_checksum()` so the two ends agree the image
/// arrived intact (belt-and-braces on top of TCP's own integrity).  A trailing
/// partial word is ignored; callers pad the image to whole words first.
#[must_use]
fn image_checksum(image: &[u8]) -> u32 {
    image
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .fold(0, u32::wrapping_add)
}

/// Build the 12-byte request header: magic, image length, entry PC (all little-endian).
fn header(image_len: usize, entry_pc: u32) -> Result<[u8; 12], NetbootError> {
    let len = u32::try_from(image_len).map_err(|_| NetbootError::ImageTooLarge(image_len))?;
    let mut hdr = [0_u8; 12];
    hdr[0..4].copy_from_slice(&NETBOOT_MAGIC.to_le_bytes());
    hdr[4..8].copy_from_slice(&len.to_le_bytes());
    hdr[8..12].copy_from_slice(&entry_pc.to_le_bytes());
    Ok(hdr)
}

/// Check the board's 8-byte acknowledgement (status, checksum; both little-endian).
///
/// Returns the board's checksum when the board accepted the image and its
/// checksum matches `host_checksum`.
fn check_ack(ack: [u8; 8], host_checksum: u32) -> Result<u32, NetbootError> {
    let status = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
    let board = u32::from_le_bytes([ack[4], ack[5], ack[6], ack[7]]);
    if status != 0 {
        return Err(NetbootError::Rejected(status));
    }
    if board != host_checksum {
        return Err(NetbootError::Checksum { host: host_checksum, board });
    }
    Ok(board)
}

/// Resolve `ip`/`port` to a socket address (IPv4, IPv6 or a host name).
fn resolve(ip: &str, port: u16) -> Result<SocketAddr, NetbootError> {
    (ip, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| NetbootError::Io(io::Error::new(io::ErrorKind::AddrNotAvailable, format!("no address found for {ip}"))))
}

/// Connect to the board and stream the DDR image, then verify the reply.
///
/// Fails if the board cannot be reached, rejects the image, or reports a
/// different checksum.
#[cfg(not(tarpaulin_include))] // Cannot test live TCP in tarpaulin
pub(crate) fn net_load(ip: &str, port: u16, image: &[u8], entry_pc: u32, msg_list: &mut Diagnostics) -> Result<(), NetbootError> {
    let hdr = header(image.len(), entry_pc)?;
    let addr = resolve(ip, port)?;
    msg_list.info(format!("netboot: connecting to {addr}"));

    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    stream.write_all(&hdr)?;
    stream.write_all(image)?;
    stream.flush()?;

    let mut ack = [0_u8; 8];
    stream.read_exact(&mut ack)?;
    let checksum = check_ack(ack, image_checksum(image))?;

    msg_list.info(format!(
        "netboot OK: {}, entry 0x{entry_pc:08X}, checksum 0x{checksum:08X}",
        human_bytes(image.len())
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_starts_with_knet_magic() {
        let hdr = header(0x1234, 0x20).unwrap();
        assert_eq!(&hdr[0..4], b"KNET");
        assert_eq!(&hdr[4..8], &0x1234_u32.to_le_bytes());
        assert_eq!(&hdr[8..12], &0x20_u32.to_le_bytes());
    }

    #[test]
    #[cfg(target_pointer_width = "64")] // a 32-bit usize cannot exceed the length field
    fn header_rejects_image_over_4_gib() {
        let too_big = u32::MAX as usize + 1;
        assert!(matches!(header(too_big, 0), Err(NetbootError::ImageTooLarge(n)) if n == too_big));
        assert!(header(u32::MAX as usize, 0).is_ok());
    }

    #[test]
    fn checksum_sums_little_endian_words() {
        assert_eq!(image_checksum(&[]), 0);
        assert_eq!(image_checksum(&[1, 0, 0, 0, 2, 0, 0, 0]), 3);
        assert_eq!(image_checksum(&[0x78, 0x56, 0x34, 0x12]), 0x1234_5678);
        // Wraps rather than overflowing.
        assert_eq!(image_checksum(&[0xFF, 0xFF, 0xFF, 0xFF, 2, 0, 0, 0]), 1);
        // A trailing partial word is not counted.
        assert_eq!(image_checksum(&[1, 0, 0, 0, 9]), 1);
    }

    #[test]
    fn ack_accepted_when_status_zero_and_checksum_matches() {
        let mut ack = [0_u8; 8];
        ack[4..8].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
        assert_eq!(check_ack(ack, 0xDEAD_BEEF).unwrap(), 0xDEAD_BEEF);
    }

    #[test]
    fn ack_with_nonzero_status_is_rejected() {
        let ack = [2, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(check_ack(ack, 0), Err(NetbootError::Rejected(2))));
    }

    #[test]
    fn ack_with_wrong_checksum_is_an_error() {
        let ack = [0, 0, 0, 0, 1, 0, 0, 0];
        assert!(matches!(check_ack(ack, 2), Err(NetbootError::Checksum { host: 2, board: 1 })));
    }

    #[test]
    fn resolve_accepts_ipv4_and_ipv6_literals() {
        assert_eq!(resolve("192.168.1.5", 5000).unwrap(), "192.168.1.5:5000".parse::<SocketAddr>().unwrap());
        assert_eq!(resolve("::1", 5000).unwrap(), "[::1]:5000".parse::<SocketAddr>().unwrap());
    }
}
