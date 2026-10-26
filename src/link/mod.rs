//! Getting a program onto the board: over the UART ([`serial`]: upload,
//! the serial monitor and the UART test runner) or over the network
//! ([`netload`]: stream a flat DDR image to the board's netboot server).

pub(crate) mod netload;
pub(crate) mod serial;
