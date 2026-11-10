//! SD fault injection (`--sd-fault SPEC`): make the emulated card fail
//! chosen data blocks or commands, so a driver's error recovery can be
//! tested end to end.
//!
//! `SPEC` is `KIND[:CMD]@N[xCOUNT]`: the `N`th event of the kind since the
//! run started (1-based) fails, and so do the `COUNT - 1` matching events
//! after it (default 1).  A fault is transient: a retry is a new event, so
//! once `COUNT` events have failed the card behaves again.  Each spec
//! counts its own matching events.
//!
//! | Kind | Event | SD mode (native) | SPI mode |
//! |---|---|---|---|
//! | `wcrc` | a memory block written (CMD24/CMD25) | CRC status token 101: `WRITE_CRC_ERR` | data response `0x0B` (`0xEB`), no busy |
//! | `wbusy` | the same | the busy never ends: `BUSY_TIMEOUT` (at once) | data response `0x05`, then MISO stays `0x00` until CS goes high or the slot is powered off |
//! | `rerr` (or `rcrc`) | a memory block read (CMD17/CMD18) | the block arrives with a bad CRC: `DATA_CRC_ERR`, nothing reaches the FIFO | error token `0x04` (card ECC failed) in place of the block; a CMD18 sends nothing more until CMD12 |
//! | `cmdto` | a command the card receives (any index, or only `CMD`) | no response: `CMD_TIMEOUT` | no response: R1 stays `0xFF` |
//!
//! A failed write is not programmed, so ACMD22 counts only the blocks
//! before it.  A command that gets no response is not executed at all (the
//! card took it as a frame with a bad CRC); it also cancels a preceding
//! CMD55.  `:CMD` filters `cmdto` by the index as sent (`cmdto:41` matches
//! ACMD41 as well as CMD41).

use std::fmt;
use std::str::FromStr;

/// What a fault does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// A written block is rejected with a CRC error.
    WriteCrc,
    /// A written block's busy never ends.
    WriteBusy,
    /// A read block arrives bad.
    ReadError,
    /// A command gets no response.
    CmdTimeout,
}

impl FaultKind {
    /// The name in a `SPEC`.
    const fn name(self) -> &'static str {
        match self {
            Self::WriteCrc => "wcrc",
            Self::WriteBusy => "wbusy",
            Self::ReadError => "rerr",
            Self::CmdTimeout => "cmdto",
        }
    }
}

/// An event a fault can match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Event {
    /// A memory block written, at this block address.
    Write(u64),
    /// A memory block read, at this block address.
    Read(u64),
    /// A command with this index.
    Command(u8),
}

/// One `--sd-fault` spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdFault {
    /// What it does.
    pub kind: FaultKind,
    /// `cmdto` only: the command index it matches (`None`: any).
    pub cmd: Option<u8>,
    /// The first matching event that fails (1-based).
    pub at: u64,
    /// How many consecutive matching events fail.
    pub count: u64,
    /// Matching events seen so far.
    seen: u64,
    /// Faults injected so far.
    injected: u64,
}

impl SdFault {
    /// Whether `event` is one this spec counts.
    const fn matches(&self, event: Event) -> bool {
        match (self.kind, event) {
            (FaultKind::WriteCrc | FaultKind::WriteBusy, Event::Write(_)) | (FaultKind::ReadError, Event::Read(_)) => true,
            (FaultKind::CmdTimeout, Event::Command(index)) => match self.cmd {
                Some(cmd) => cmd == index,
                None => true,
            },
            _ => false,
        }
    }

    /// Count `event`; true if it is one of those that fail.
    fn hit(&mut self, event: Event) -> bool {
        if !self.matches(event) {
            return false;
        }
        self.seen += 1;
        let hit = self.seen >= self.at && self.seen - self.at < self.count;
        if hit {
            self.injected += 1;
        }
        hit
    }
}

impl fmt::Display for SdFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind.name())?;
        if let Some(cmd) = self.cmd {
            write!(f, ":{cmd}")?;
        }
        write!(f, "@{}", self.at)?;
        if self.count != 1 {
            write!(f, "x{}", self.count)?;
        }
        Ok(())
    }
}

impl FromStr for SdFault {
    type Err = String;

    /// Parse `KIND[:CMD]@N[xCOUNT]`.
    fn from_str(spec: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("bad SD fault '{spec}': {why} (expected KIND[:CMD]@N[xCOUNT], KIND = wcrc, wbusy, rerr or cmdto)");
        let (head, tail) = spec.split_once('@').ok_or_else(|| bad("no '@'"))?;
        let (name, cmd) = match head.split_once(':') {
            Some((name, cmd)) => (name, Some(cmd)),
            None => (head, None),
        };
        let kind = match name {
            "wcrc" => FaultKind::WriteCrc,
            "wbusy" => FaultKind::WriteBusy,
            "rerr" | "rcrc" => FaultKind::ReadError,
            "cmdto" => FaultKind::CmdTimeout,
            _ => return Err(bad("unknown kind")),
        };
        let cmd = match cmd {
            None => None,
            Some(_) if kind != FaultKind::CmdTimeout => return Err(bad("only cmdto takes a command index")),
            Some(cmd) => match cmd.parse::<u8>() {
                Ok(index) if index < 64 => Some(index),
                _ => return Err(bad("the command index must be 0-63")),
            },
        };
        let (at, count) = match tail.split_once('x') {
            Some((at, count)) => (at, Some(count)),
            None => (tail, None),
        };
        let at = at
            .parse::<u64>()
            .ok()
            .filter(|&n| n >= 1)
            .ok_or_else(|| bad("N must be a number from 1"))?;
        let count = match count {
            None => 1,
            Some(count) => count
                .parse::<u64>()
                .ok()
                .filter(|&n| n >= 1)
                .ok_or_else(|| bad("COUNT must be a number from 1"))?,
        };
        Ok(Self {
            kind,
            cmd,
            at,
            count,
            seen: 0,
            injected: 0,
        })
    }
}

/// The card's fault specs.
#[derive(Debug, Clone, Default)]
pub(super) struct Faults {
    /// The specs, in command-line order.
    specs: Vec<SdFault>,
}

impl Faults {
    /// Faults from `specs`.
    pub(super) const fn new(specs: Vec<SdFault>) -> Self {
        Self { specs }
    }

    /// The fault [`Faults::check`] would inject for `event` now, without
    /// counting it.
    pub(super) fn peek(&self, event: Event) -> Option<FaultKind> {
        self.specs
            .iter()
            .find(|spec| spec.matches(event) && spec.seen + 1 >= spec.at && spec.seen + 1 - spec.at < spec.count)
            .map(|spec| spec.kind)
    }

    /// Count `event` against every spec, and return the fault to inject,
    /// if any (the first spec that hits), logging it on stderr.
    pub(super) fn check(&mut self, event: Event) -> Option<FaultKind> {
        if self.specs.is_empty() {
            return None;
        }
        let mut injected = None;
        for spec in &mut self.specs {
            if spec.hit(event) && injected.is_none() {
                injected = Some(spec.kind);
                let what = match event {
                    Event::Write(lba) => format!("write block {} (block address {lba})", spec.seen),
                    Event::Read(lba) => format!("read block {} (block address {lba})", spec.seen),
                    Event::Command(index) => format!("command {} (CMD{index})", spec.seen),
                };
                eprintln!("klausscc sd: injected {} at {what}", spec.kind.name());
            }
        }
        injected
    }

    /// The faults injected, one `spec: n` per spec, or `None` without specs.
    pub(super) fn summary(&self) -> Option<String> {
        if self.specs.is_empty() {
            return None;
        }
        let total: u64 = self.specs.iter().map(|s| s.injected).sum();
        let each: Vec<String> = self.specs.iter().map(|s| format!("{s}: {}", s.injected)).collect();
        Some(format!("SD faults injected: {total} ({})", each.join(", ")))
    }
}
