//! Network program loader — stream a flat DDR image to the board over TCP.
//!
//! The board runs `netboot.c` (a resident lwIP program) listening on a TCP
//! port.  We connect, send a 12-byte header (`magic`, `img_len`, `entry_pc`,
//! all little-endian) followed by the raw DDR image bytes, then read an 8-byte
//! acknowledgement (`status`, `checksum`).  TCP itself provides the ordering /
//! loss / retransmit that the kbt-over-UART path never needed and the earlier
//! hand-rolled UDP design would have had to implement.

use crate::helper::human_bytes;
use crate::messages::{MessageType, MsgList};
use core::time::Duration;
use std::io::{Error, Read as _, Write as _};
use std::net::TcpStream;

/// Protocol magic — `b"KNET"` read as a little-endian u32 (matches the board's
/// `NETBOOT_MAGIC`).
const NETBOOT_MAGIC: u32 = 0x5445_4E4B;

/// Default TCP port the board's netboot server listens on.
pub const NETBOOT_DEFAULT_PORT: u16 = 5000;

/// 32-bit additive checksum over little-endian 32-bit words.
///
/// Must match the board's `image_checksum()` so the two ends agree the image
/// arrived intact (belt-and-braces on top of TCP's own integrity).
#[must_use]
fn image_checksum(image: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 4 <= image.len() {
        sum = sum.wrapping_add(u32::from_le_bytes([
            image[i],
            image[i + 1],
            image[i + 2],
            image[i + 3],
        ]));
        i += 4;
    }
    sum
}

/// Connect to the board and stream the DDR image, then verify the reply.
#[cfg(not(tarpaulin_include))] // Cannot test live TCP in tarpaulin
pub fn net_load(
    ip: &str,
    port: u16,
    image: &[u8],
    entry_pc: u32,
    msg_list: &mut MsgList,
) -> Result<(), Error> {
    let addr = format!("{ip}:{port}");
    msg_list.push(
        format!("netboot: connecting to {addr}"),
        None,
        None,
        MessageType::Information,
    );

    let mut stream = TcpStream::connect(&addr)?;
    let _ = stream.set_nodelay(true);
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;

    // 12-byte header: magic, img_len, entry_pc (all little-endian).
    let mut hdr = Vec::with_capacity(12);
    hdr.extend_from_slice(&NETBOOT_MAGIC.to_le_bytes());
    hdr.extend_from_slice(&(image.len() as u32).to_le_bytes());
    hdr.extend_from_slice(&entry_pc.to_le_bytes());

    stream.write_all(&hdr)?;
    stream.write_all(image)?;
    stream.flush()?;

    let host_cks = image_checksum(image);

    // 8-byte acknowledgement: status, checksum (both little-endian).
    let mut ack = [0_u8; 8];
    stream.read_exact(&mut ack)?;
    let status = u32::from_le_bytes([ack[0], ack[1], ack[2], ack[3]]);
    let board_cks = u32::from_le_bytes([ack[4], ack[5], ack[6], ack[7]]);

    if status != 0 {
        msg_list.push(
            format!("Board rejected image (status {status})"),
            None,
            None,
            MessageType::Error,
        );
    } else if board_cks != host_cks {
        msg_list.push(
            format!("Checksum mismatch: host 0x{host_cks:08X}, board 0x{board_cks:08X}"),
            None,
            None,
            MessageType::Error,
        );
    } else {
        msg_list.push(
            format!(
                "netboot OK: {}, entry 0x{entry_pc:08X}, checksum 0x{board_cks:08X}",
                human_bytes(image.len())
            ),
            None,
            None,
            MessageType::Information,
        );
    }
    Ok(())
}
