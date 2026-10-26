//! Tests of the board debugger against [`FakeDm`], a model of the debug
//! module behind the [`Dmi`] seam: no JTAG cable or libftdi needed.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// What a [`FakeDm`] holds: the core, its memory and the debug module's
/// registers.  Shared, so a test can change it while a target owns the DM.
#[derive(Default)]
struct DmState {
    regs: [u64; KlaussRegId::COUNT],
    data0: u64,
    mem_addr: u32,
    /// Doubleword address -> its 8 bytes.
    mem: BTreeMap<u32, u64>,
    /// BP0.. and WP0.. registers, by DMI address.
    slots: BTreeMap<u8, u64>,
    watch_addr: u64,
    halted: bool,
    /// The `DBG_CAUSE_*` value of the last halt.
    cause: u8,
    halted_seen: bool,
    error: bool,
    /// Every DMCONTROL write, in order.
    controls: Vec<u64>,
    /// Memory commands on doublewords at and above this address fail.
    bad_mem_from: Option<u32>,
    /// A batch this matches fails as if the cable had been pulled.
    lost: Option<fn(&[DmiReq]) -> bool>,
}

impl DmState {
    /// Halt the core for `cause`.
    const fn halt(&mut self, cause: Cause) {
        self.halted = true;
        self.halted_seen = true;
        self.cause = cause.code();
    }

    fn status(&mut self) -> u64 {
        let st = (0x4B << 24)
            | ((HW_WATCHPOINTS as u64) << 20)
            | ((HW_BREAKPOINTS as u64) << 16)
            | (1 << 12)
            | (u64::from(self.halted_seen) << 11)
            | (u64::from(self.cause) << 4)
            | (u64::from(self.error) << 3)
            | u64::from(self.halted);
        self.halted_seen = false;
        st
    }

    fn control(&mut self, v: u64) {
        self.controls.push(v);
        if v & CTL_HALTREQ != 0 && !self.halted {
            self.halt(Cause::Request);
        }
        if v & CTL_RESUMEREQ != 0 && self.halted {
            self.halted = false;
            if v & CTL_STEP != 0 {
                self.regs[17] += 4;
                self.halt(Cause::Step);
            }
        }
    }

    fn mem_cmd(&mut self, v: u64) {
        if self.bad_mem_from.is_some_and(|bad| self.mem_addr >= bad) {
            self.error = true;
        } else if v & MEM_WRITE != 0 {
            let mut bytes = self.mem.get(&self.mem_addr).copied().unwrap_or(0).to_le_bytes();
            for (lane, (b, new)) in bytes.iter_mut().zip(self.data0.to_le_bytes()).enumerate() {
                if v & (1 << lane) != 0 {
                    *b = new;
                }
            }
            self.mem.insert(self.mem_addr, u64::from_le_bytes(bytes));
        } else {
            self.data0 = self.mem.get(&self.mem_addr).copied().unwrap_or(0);
        }
        if v & MEM_INC != 0 {
            self.mem_addr += 8;
        }
    }

    fn read(&mut self, addr: u8) -> u64 {
        match addr {
            DMSTATUS => self.status(),
            DATA0 => self.data0,
            WATCH_ADDR => self.watch_addr,
            _ => self.slots.get(&addr).copied().unwrap_or(0),
        }
    }

    fn write(&mut self, addr: u8, v: u64) {
        match addr {
            DMCONTROL => self.control(v),
            DMSTATUS => self.error = false,
            REG_CMD if v & REG_CMD_WRITE != 0 => self.regs[(v & 0xFF) as usize] = self.data0,
            REG_CMD => self.data0 = self.regs[(v & 0xFF) as usize],
            DATA0 => self.data0 = v,
            MEM_ADDR => self.mem_addr = v as u32 & !7,
            MEM_CMD => self.mem_cmd(v),
            _ => {
                self.slots.insert(addr, v);
            }
        }
    }

    /// The word at `addr` (4-byte aligned).
    fn word(&self, addr: u32) -> u32 {
        let dw = self.mem.get(&(addr & !7)).copied().unwrap_or(0);
        (dw >> ((addr & 4) * 8)) as u32
    }

    /// Store the word at `addr` (4-byte aligned).
    fn set_word(&mut self, addr: u32, w: u32) {
        let dw = self.mem.entry(addr & !7).or_default();
        let shift = (addr & 4) * 8;
        *dw = (*dw & !(0xFFFF_FFFF << shift)) | (u64::from(w) << shift);
    }
}

/// A model of the debug module: answers DMI requests from a [`DmState`].
#[derive(Clone, Default)]
struct FakeDm(Rc<RefCell<DmState>>);

impl Dmi for FakeDm {
    fn dmi_batch(&mut self, reqs: &[DmiReq]) -> crate::jtag::Result<Vec<u64>> {
        let mut dm = self.0.borrow_mut();
        if dm.lost.is_some_and(|lost| lost(reqs)) {
            return Err(JtagError::ReadTimeout);
        }
        Ok(reqs
            .iter()
            .map(|req| match *req {
                DmiReq::Read(addr) => dm.read(addr),
                DmiReq::Write(addr, v) => {
                    dm.write(addr, v);
                    0
                }
                DmiReq::Nop => 0,
            })
            .collect())
    }
}

/// A halted core at `pc`, attached, and a handle on its debug module.
fn halted_target(pc: u32) -> (BoardTarget<FakeDm>, Rc<RefCell<DmState>>) {
    let dm = FakeDm::default();
    let state = Rc::clone(&dm.0);
    state.borrow_mut().regs[17] = u64::from(pc);
    state.borrow_mut().halt(Cause::Request);
    let mut board = Board::new(dm);
    assert_eq!(attach(&mut board, None).ok(), Some(pc));
    state.borrow_mut().controls.clear();
    (BoardTarget::new(board), state)
}

/// The kind of a target error.
const fn describe(e: &TargetError<BoardError>) -> &'static str {
    match e {
        TargetError::Fatal(_) => "fatal",
        TargetError::Errno(14) => "efault",
        TargetError::NonFatal => "nonfatal",
        _ => "other",
    }
}

/// The value of a target call that should succeed.
fn ok<T>(result: TargetResult<T, BoardTarget<FakeDm>>) -> T {
    match result {
        Ok(v) => v,
        Err(e) => panic!("target error: {}", describe(&e)),
    }
}

/// How a target call failed (`"ok"` if it did not).
fn err_kind<T>(result: &TargetResult<T, BoardTarget<FakeDm>>) -> &'static str {
    result.as_ref().map_or_else(describe, |_| "ok")
}

#[test]
fn errors_map_to_target_errors_by_kind() {
    let kind = |e: BoardError| match TargetError::from(e) {
        TargetError::Fatal(_) => "fatal",
        TargetError::Errno(14) => "efault",
        TargetError::NonFatal => "nonfatal",
        _ => "other",
    };
    assert_eq!(
        kind(BoardError::MemCommand {
            status: DmStatus::from_raw(8)
        }),
        "efault"
    );
    assert_eq!(kind(BoardError::PastTop { addr: 0xFFFF_FFFF, len: 2 }), "efault");
    assert_eq!(kind(BoardError::Jtag(JtagError::ReadTimeout)), "fatal");
    assert_eq!(
        kind(BoardError::Lost(Box::new(BoardError::NotHalted {
            status: DmStatus::from_raw(0)
        }))),
        "fatal"
    );
    assert_eq!(kind(BoardError::Jtag(JtagError::DmiBusy { addr: 5 })), "nonfatal");
    assert_eq!(kind(BoardError::Unsupported("no")), "nonfatal");
}

#[test]
fn errors_keep_their_messages() {
    assert_eq!(
        BoardError::WrongCause { cause: Cause::Trap }.to_string(),
        "the core halted for cause 4 instead of at the entry"
    );
    assert_eq!(
        BoardError::MemCommand {
            status: DmStatus::from_raw(0x4B24_0009)
        }
        .to_string(),
        "debug module memory command failed (DMSTATUS 0x4B240009)"
    );
    assert_eq!(
        BoardError::HaltRequest(Box::new(BoardError::NotHalted {
            status: DmStatus::from_raw(0x4B00_0000)
        }))
        .to_string(),
        "the core did not halt (DMSTATUS 0x4B000000): is a program running on the board? (load one with -s)"
    );
    assert_eq!(
        past_top(0xFFFF_FFFE, 4).to_string(),
        "4 bytes at 0xFFFFFFFE run past the end of the address space"
    );
    assert_eq!(BoardError::from(JtagError::ReadTimeout).to_string(), "ftdi read timeout");
    let load = BoardError::Load {
        path: "prog.elf".to_owned(),
        port: "/dev/ttyUSB1".to_owned(),
        source: Box::new(crate::error::Error::Assembly(1)),
    };
    assert_eq!(load.to_string(), "loading prog.elf over /dev/ttyUSB1 failed (completed with 1 error)");
}

#[test]
fn attach_halts_a_running_program_and_clears_stale_slots() {
    let dm = FakeDm::default();
    let state = Rc::clone(&dm.0);
    state.borrow_mut().regs[17] = 0x1234;
    state.borrow_mut().slots.insert(BP0 + 1, SLOT_ENABLE | 0x40);
    state.borrow_mut().slots.insert(WP0, SLOT_ENABLE);
    let mut board = Board::new(dm);
    assert_eq!(attach(&mut board, None).ok(), Some(0x1234));
    let dm = state.borrow();
    assert_eq!(dm.controls, [CTL_ATTACHED | CTL_HALTREQ, CTL_ATTACHED]);
    assert!(dm.slots.values().all(|&v| v == 0), "a killed server's slots are disarmed");
    assert_eq!(dm.slots.len(), HW_BREAKPOINTS + HW_WATCHPOINTS);
}

#[test]
fn attach_leaves_a_halted_core_where_it_is() {
    let (_, state) = halted_target(0x80);
    let dm = FakeDm(Rc::clone(&state));
    let mut board = Board::new(dm);
    assert_eq!(attach(&mut board, None).ok(), Some(0x80));
    assert_eq!(state.borrow().controls, [CTL_ATTACHED], "no halt request");
}

#[test]
fn attach_with_a_load_waits_for_the_halt_at_the_entry() {
    let dm = FakeDm::default();
    let state = Rc::clone(&dm.0);
    let mut board = Board::new(dm);
    let loader = Rc::clone(&state);
    let mut load = move || {
        let mut dm = loader.borrow_mut();
        assert_eq!(dm.controls.last(), Some(&(CTL_ATTACHED | CTL_HALT_ON_LOAD)));
        dm.regs[17] = 0x20;
        dm.halt(Cause::Request);
        Ok(())
    };
    assert_eq!(attach(&mut board, Some(&mut load)).ok(), Some(0x20));
    assert_eq!(state.borrow().controls, [CTL_ATTACHED | CTL_HALT_ON_LOAD, CTL_ATTACHED]);
}

#[test]
fn attach_with_a_load_rejects_a_halt_for_another_cause() {
    let dm = FakeDm::default();
    let state = Rc::clone(&dm.0);
    let mut board = Board::new(dm);
    let mut load = move || {
        state.borrow_mut().halt(Cause::Breakpoint);
        Ok(())
    };
    let err = attach(&mut board, Some(&mut load)).err();
    assert!(matches!(err, Some(BoardError::WrongCause { cause: Cause::Breakpoint })), "{err:?}");
}

#[test]
fn attach_passes_on_a_failed_load() {
    let mut board = Board::new(FakeDm::default());
    let mut load = || Err(BoardError::Unsupported("no port"));
    let err = attach(&mut board, Some(&mut load)).err();
    assert!(matches!(err, Some(BoardError::Unsupported("no port"))), "{err:?}");
}

#[test]
fn registers_read_and_write_through_reg_cmd() {
    let (mut target, state) = halted_target(0x100);
    for (i, r) in state.borrow_mut().regs.iter_mut().enumerate() {
        *r = 0x1111 * i as u64;
    }
    let mut regs = KlaussRegs::default();
    ok(target.read_registers(&mut regs));
    assert_eq!(regs.r[3], 0x3333);
    assert_eq!((regs.sp, regs.pc, regs.flags, regs.int_mask), (0x11110, 0x12221, 0x13332, 0x14443));

    regs.r[0] = 0xDEAD_BEEF_0000_0001;
    regs.pc = 0x200;
    ok(target.write_registers(&regs));
    assert_eq!(state.borrow().regs[0], 0xDEAD_BEEF_0000_0001);
    assert_eq!(state.borrow().regs[17], 0x200);
    assert_eq!(state.borrow().regs[19], 0x14443);
}

#[test]
fn single_registers_read_and_write_by_size() {
    let (mut target, state) = halted_target(0x0102_0304);
    state.borrow_mut().regs[5] = 0x0807_0605_0403_0201;
    let mut buf = [0_u8; 8];
    assert_eq!(ok(target.read_register((), KlaussRegId::Gpr(5), &mut buf)), 8);
    assert_eq!(buf, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(ok(target.read_register((), KlaussRegId::Pc, &mut buf)), 4);
    assert_eq!(buf[..4], [4, 3, 2, 1]);

    ok(target.write_register((), KlaussRegId::Sp, &[0x20, 0xFF, 0x7F, 0x07]));
    assert_eq!(state.borrow().regs[16], 0x077F_FF20);
    assert_eq!(err_kind(&target.write_register((), KlaussRegId::Sp, &[1, 2])), "nonfatal");
}

#[test]
fn memory_reads_and_writes_any_alignment() {
    let (mut target, state) = halted_target(0);
    for dw in [0x100, 0x108, 0x110] {
        state.borrow_mut().mem.insert(dw, u64::MAX);
    }
    let bytes: Vec<u8> = (1..=20).collect();
    ok(target.write_addrs(0x103, &bytes));
    let mut back = [0_u8; 20];
    assert_eq!(ok(target.read_addrs(0x103, &mut back)), 20);
    assert_eq!(back[..], bytes[..]);
    let dm = state.borrow();
    assert_eq!(dm.word(0x100), 0x01FF_FFFF, "bytes below the write are kept");
    assert_eq!(dm.mem.get(&0x110).copied(), Some(0xFF14_1312_1110_0F0E), "bytes above it too");
}

#[test]
fn memory_the_debug_module_refuses_is_efault() {
    let (mut target, state) = halted_target(0);
    state.borrow_mut().bad_mem_from = Some(0x1000);
    let mut buf = [0_u8; 4];
    assert_eq!(err_kind(&target.read_addrs(0x1000, &mut buf)), "efault");
    assert!(!state.borrow().error, "the sticky error is cleared");
    assert_eq!(err_kind(&target.write_addrs(0xFF8, &[0; 16])), "efault");
    assert_eq!(err_kind(&target.read_addrs(0xFFFF_FFFE, &mut buf)), "efault", "past the top");
    assert_eq!(ok(target.read_addrs(0xFF8, &mut buf)), 4);
}

#[test]
fn mmio_is_off_limits_until_enabled() {
    let (mut target, _) = halted_target(0);
    let mut buf = [0_u8; 4];
    assert_eq!(err_kind(&target.read_addrs(0xF000_0000, &mut buf)), "efault");
    assert_eq!(err_kind(&target.read_addrs(0xEFFF_FFFE, &mut buf)), "efault", "ends in MMIO");
    target.allow_mmio = true;
    assert_eq!(ok(target.read_addrs(0xF000_0000, &mut buf)), 4);
}

#[test]
fn software_breakpoints_patch_a_trap_and_hide_it() {
    let (mut target, state) = halted_target(0);
    state.borrow_mut().set_word(0x104, 0xAABB_CCDD);
    assert!(ok(target.add_sw_breakpoint(0x104, 4)));
    assert!(ok(target.add_sw_breakpoint(0x104, 4)), "adding twice is fine");
    assert_eq!(state.borrow().word(0x104), TRAP_WORD);
    let mut buf = [0_u8; 6];
    ok(target.read_addrs(0x102, &mut buf));
    assert_eq!(buf, [0, 0, 0xDD, 0xCC, 0xBB, 0xAA], "reads show the original word");
    assert!(!ok(target.add_sw_breakpoint(0x106, 4)), "misaligned");
    assert!(!ok(target.add_sw_breakpoint(0xF000_0000, 4)), "MMIO");
    assert!(ok(target.remove_sw_breakpoint(0x104, 4)));
    assert!(!ok(target.remove_sw_breakpoint(0x104, 4)));
    assert_eq!(state.borrow().word(0x104), 0xAABB_CCDD);
}

#[test]
fn hardware_breakpoints_fill_the_slots() {
    let (mut target, state) = halted_target(0);
    for i in 0..HW_BREAKPOINTS as u32 {
        assert!(ok(target.add_hw_breakpoint(0x100 + 4 * i, 4)));
    }
    assert!(ok(target.add_hw_breakpoint(0x100, 4)), "already set");
    assert!(!ok(target.add_hw_breakpoint(0x200, 4)), "out of slots");
    assert_eq!(state.borrow().slots.get(&(BP0 + 1)).copied(), Some(SLOT_ENABLE | 0x104));
    assert!(ok(target.remove_hw_breakpoint(0x104, 4)));
    assert!(!ok(target.remove_hw_breakpoint(0x104, 4)));
    assert_eq!(state.borrow().slots.get(&(BP0 + 1)).copied(), Some(0));
    assert!(ok(target.add_hw_breakpoint(0x200, 4)), "the freed slot is reused");
    assert_eq!(state.borrow().slots.get(&(BP0 + 1)).copied(), Some(SLOT_ENABLE | 0x200));
}

#[test]
fn watchpoints_arm_lanes_of_one_doubleword() {
    let (mut target, state) = halted_target(0);
    assert!(ok(target.add_hw_watchpoint(0x102, 2, WatchKind::Read)));
    assert_eq!(
        state.borrow().slots.get(&WP0).copied(),
        Some(0x100 | SLOT_ENABLE | WP_READ | (0b1100 << WP_LANES_SHIFT))
    );
    assert!(!ok(target.add_hw_watchpoint(0x106, 4, WatchKind::Write)), "crosses a doubleword");
    assert!(!ok(target.add_hw_watchpoint(0x100, 0, WatchKind::Write)));
    assert!(ok(target.add_hw_watchpoint(0x108, 8, WatchKind::ReadWrite)));
    assert!(!ok(target.add_hw_watchpoint(0x110, 4, WatchKind::Write)), "out of slots");
    assert!(ok(target.remove_hw_watchpoint(0x102, 2, WatchKind::Read)));
    assert!(!ok(target.remove_hw_watchpoint(0x102, 2, WatchKind::Write)), "kind must match");
    assert_eq!(state.borrow().slots.get(&WP0).copied(), Some(0));
}

#[test]
fn a_watchpoint_halt_reports_the_access() {
    let (mut target, state) = halted_target(0);
    assert!(ok(target.add_hw_watchpoint(0x104, 4, WatchKind::Write)));
    assert!(target.resume(None).is_ok());
    state.borrow_mut().watch_addr = 0x106;
    state.borrow_mut().halt(Cause::Watchpoint);
    let stop = target.poll().ok().flatten();
    assert_eq!(
        stop,
        Some(SingleThreadStopReason::Watch {
            tid: (),
            kind: WatchKind::Write,
            addr: 0x106
        })
    );
}

#[test]
fn resume_runs_until_the_core_halts() {
    let (mut target, state) = halted_target(0x100);
    assert!(target.resume(None).is_ok());
    assert_eq!(state.borrow().controls, [CTL_ATTACHED | CTL_RESUMEREQ]);
    assert_eq!(target.poll().ok(), Some(None), "still running");
    state.borrow_mut().halt(Cause::Breakpoint);
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::HwBreak(()))));
    assert!(target.resume(Some(Signal::SIGINT)).is_err(), "no signals");
}

#[test]
fn a_step_reports_done() {
    let (mut target, state) = halted_target(0x100);
    assert!(target.step(None).is_ok());
    assert_eq!(state.borrow().controls, [CTL_ATTACHED | CTL_RESUMEREQ | CTL_STEP]);
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::DoneStep)));
    assert_eq!(state.borrow().regs[17], 0x104);
}

#[test]
fn stepping_a_compiled_in_trap_skips_it() {
    let (mut target, state) = halted_target(0x100);
    state.borrow_mut().set_word(0x100, TRAP_WORD);
    assert!(target.step(None).is_ok());
    assert!(state.borrow().controls.is_empty(), "the skip is the step");
    assert_eq!(state.borrow().regs[17], 0x104);
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::DoneStep)));
}

#[test]
fn traps_are_breakpoints_only_where_the_debugger_set_them() {
    let (mut target, state) = halted_target(0x100);
    assert!(ok(target.add_sw_breakpoint(0x108, 4)));
    assert!(target.resume(None).is_ok());
    state.borrow_mut().regs[17] = 0x108;
    state.borrow_mut().halt(Cause::Trap);
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::SwBreak(()))));
    assert!(target.resume(None).is_ok(), "resuming over the patched breakpoint");
    state.borrow_mut().regs[17] = 0x200;
    state.borrow_mut().halt(Cause::Trap);
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::Signal(Signal::SIGTRAP))));
}

#[test]
fn an_interrupt_halts_the_core() {
    let (mut target, state) = halted_target(0x100);
    assert!(target.resume(None).is_ok());
    assert_eq!(target.on_interrupt().ok(), Some(Some(SingleThreadStopReason::Signal(Signal::SIGINT))));
    assert_eq!(state.borrow().controls.last(), Some(&(CTL_ATTACHED | CTL_HALTREQ)));
}

#[test]
fn detach_restores_memory_clears_slots_and_lets_the_program_run() {
    let (mut target, state) = halted_target(0x100);
    state.borrow_mut().set_word(0x104, 0x1234_5678);
    assert!(ok(target.add_sw_breakpoint(0x104, 4)));
    assert!(ok(target.add_hw_breakpoint(0x200, 4)));
    assert!(ok(target.add_hw_watchpoint(0x300, 4, WatchKind::Write)));
    assert!(target.detach().is_ok());
    let dm = state.borrow();
    assert_eq!(dm.word(0x104), 0x1234_5678);
    assert!(dm.slots.values().all(|&v| v == 0));
    assert_eq!(dm.controls, [CTL_RESUMEREQ], "attached cleared, running on");
    assert!(target.sw_breakpoints.is_empty());
}

#[test]
fn detach_halts_a_running_core_first() {
    let (mut target, state) = halted_target(0x100);
    assert!(target.resume(None).is_ok());
    assert!(target.detach().is_ok());
    assert_eq!(
        state.borrow().controls,
        [CTL_ATTACHED | CTL_RESUMEREQ, CTL_ATTACHED | CTL_HALTREQ, CTL_RESUMEREQ]
    );
}

#[test]
fn detach_leaves_a_halted_program_halted() {
    let (mut target, state) = halted_target(0x100);
    state.borrow_mut().regs[12] = 0x1_0007;
    assert!(target.resume(None).is_ok());
    state.borrow_mut().halt(Cause::Halt);
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::Exited(7))));
    assert!(target.detach().is_ok());
    assert_eq!(state.borrow().controls.last(), Some(&0), "only attached cleared");
}

#[test]
fn detach_is_best_effort_when_the_status_read_fails() {
    let (mut target, state) = halted_target(0x100);
    state.borrow_mut().set_word(0x104, 0x1234_5678);
    assert!(ok(target.add_sw_breakpoint(0x104, 4)));
    assert!(ok(target.add_hw_breakpoint(0x200, 4)));
    state.borrow_mut().lost = Some(|reqs| reqs == [DmiReq::Read(DMSTATUS)]);
    let err = target.detach().err();
    assert!(matches!(err, Some(BoardError::Jtag(JtagError::ReadTimeout))), "{err:?}");
    let dm = state.borrow();
    assert_eq!(dm.word(0x104), 0x1234_5678, "the TRAP is still removed");
    assert!(dm.slots.values().all(|&v| v == 0), "the slots are still cleared");
    assert_eq!(dm.controls, [0], "without the cause, the core is not resumed");
}

#[test]
fn detach_is_best_effort_when_restoring_memory_fails() {
    let (mut target, state) = halted_target(0x100);
    assert!(ok(target.add_sw_breakpoint(0x104, 4)));
    assert!(ok(target.add_sw_breakpoint(0x108, 4)));
    assert!(ok(target.add_hw_breakpoint(0x200, 4)));
    state.borrow_mut().bad_mem_from = Some(0x104);
    let err = target.detach().err();
    assert!(matches!(err, Some(BoardError::MemCommand { .. })), "{err:?}");
    let dm = state.borrow();
    assert!(dm.slots.values().all(|&v| v == 0), "the slots are still cleared");
    assert_eq!(dm.controls, [CTL_RESUMEREQ], "and the program runs on");
    assert!(target.sw_breakpoints.is_empty(), "every breakpoint was tried once");
}

#[test]
fn a_lost_link_ends_the_session() {
    let (mut target, state) = halted_target(0x100);
    assert!(target.resume(None).is_ok());
    state.borrow_mut().lost = Some(|_| true);
    let err = target.run_until_event(|| false).err();
    assert!(
        matches!(&err, Some(BoardError::Lost(e)) if matches!(**e, BoardError::Jtag(JtagError::ReadTimeout))),
        "{err:?}"
    );
    let mut regs = KlaussRegs::default();
    assert_eq!(err_kind(&target.read_registers(&mut regs)), "fatal");
}

#[test]
fn the_run_state_follows_resume_stop_and_exit() {
    let (mut target, state) = halted_target(0x100);
    assert_eq!(target.state, RunState::Halted);
    assert!(target.resume(None).is_ok());
    assert_eq!(target.state, RunState::Running { halt_seen: false });
    // the halt and its "halted since" bit arrive in one read
    state.borrow_mut().halt(Cause::Breakpoint);
    assert!(target.poll().ok().flatten().is_some());
    assert_eq!(target.state, RunState::Halted);
    state.borrow_mut().set_word(0x100, TRAP_WORD);
    assert!(target.step(None).is_ok());
    assert_eq!(target.state, RunState::SkippedTrap);
    assert!(target.state.is_running(), "detach still halts it");
    assert!(target.poll().ok().flatten().is_some());
    assert!(target.resume(None).is_ok());
    state.borrow_mut().halt(Cause::Halt);
    assert!(target.poll().ok().flatten().is_some());
    assert_eq!(target.state, RunState::Exited);
}

#[test]
fn a_halt_seen_before_the_core_stops_is_remembered() {
    let (mut target, state) = halted_target(0x100);
    assert!(target.resume(None).is_ok());
    // the core halted and was resumed by something else before the poll
    state.borrow_mut().halted_seen = true;
    assert_eq!(target.poll().ok(), Some(None));
    assert_eq!(target.state, RunState::Running { halt_seen: true });
    state.borrow_mut().halted = true;
    state.borrow_mut().cause = Cause::Breakpoint.code();
    assert_eq!(target.poll().ok(), Some(Some(SingleThreadStopReason::HwBreak(()))));
}
