# Hardware test handoff: klausscc `review-fixes`

For whoever runs klausscc against the Nexys A7: the Claude session on the
Linux machine with the board attached, or Graham. It records what has been
verified on hardware (Phase A) and what still needs a board run (Phase B).

## Setup on the test machine

| | |
|---|---|
| Host | Linux, rustc 1.99.1, libftdi1 1.6rc1 (pkg-config) |
| Board | Nexys A7-100T, FT2232H: `/dev/ttyUSB0` = JTAG (interface A), `/dev/ttyUSB1` = UART (interface B) |
| Bitstream | KlaussCPU `cf01863`, DEBUG_EN=1, in QSPI flash (survives a power cycle) and loaded over JTAG. Its debug module also clears haltreq on a plain load. |
| Old flash image | `~/.cache/klausscpu-debug-build/qspi_backup_2026-10-26.bin` |

```sh
cd ~/Documents/src/klausscc
git pull --ff-only && git log -1 --oneline
cargo build --release --features board
cargo test --features board && cargo test
alias kc=./target/release/klausscc
```

Bare `-s` auto-detects the UART: it accepts only FTDI ports and picks the
highest name, which here is `/dev/ttyUSB1`.

## Phase A: done (klausscc 024d940, bitstream 38bf920)

All checks passed except the two findings below:

- `cargo test --features board` (first run with libftdi) passed.
- Serial upload and monitor work: the terminal is restored, auto-detect works,
  a bad port gives an error, typed keys arrive once each, and the monitor
  exits on HALT.
- Exit codes 0, 1, 2 and 3 are all correct.
- `-L` passed 20/21 (test_io skipped).
- A C ELF prints the same output on the emulator and the board.
- The whole `--gdbserver --board` checklist passed: registers, memory, batched
  register writes, software and hardware breakpoints (4 bp and 2 wp slots read
  from DMSTATUS), stepping, detach with cleanup, attach without `-s`, and
  `--jtag-khz 1000`.

Not run: pulling the cable mid-session, and netload.

Findings, now fixed:
- **F1:** a failed attach to a core that had already stopped left
  DMCONTROL `HALTREQ` set, so the next plain load froze on its first
  instruction. Fixed in klausscc (`ac1bd4d`, a clear "already stopped" error)
  and in the RTL (`cf01863`).
- **F2:** `v2_hello.kla` wrote TX_DATA without polling TX_BUSY. On hardware the
  loader's 18-byte "Load Complete OK" reply is still being sent when the
  program starts, and a write while busy is **dropped**. The emulator now
  models TX busy time: 334 cycles per store, and the loader reply at start.
  `--emulate` reports a write while busy, and `--emulate-test` fails the
  file. All fixtures now poll before writing.

## Phase B: done (klausscc 8674f21, bitstream cf01863)

Every item passed and there were **no emulator/hardware mismatches**:
- hw_alignment passed 102/102 on the board.
- `-L` matched `--emulate-test` file for file (25/26, with 1 skipped).
- The busy-write drop matched exactly: the emulator and the board both drop
  all 3 unpolled bytes.
- The C runtime, byte strings and the F1/F2 retests all passed.
- `run_board_smoke.sh` passed.

Netload was not run.

One finding, now fixed:
- **F3:** `-T`/`-L` consumed the loader's reply with a single read. A program
  that prints immediately (test_directives) sends its values in the same read,
  so they were lost and the test timed out. The test runners now leave the
  reply to the test monitor, which already skips the "… Complete" line
  (`AwaitAck::Skip`). Patch from the test session, verified on the board.

The plan below is kept for re-runs.

## Phase B plan

Since Phase A the emulator has been aligned with `pipeline_core.sv`: flags
for logic/MUL/DIV/MOD/ABS/rotates and F-bit ops, the full legality decode,
unaligned 64-bit accesses, DELAY time and TX busy. The assembler has changed
too:
- `#name` strings are now plain NUL-terminated bytes, and `#name` points at
  the first character.
- `.S` immediates are range-checked.
- Inline data is 8-byte aligned.
- `runtime/uart_stubs.kla` is v2, so `libc.kla`, `test2.kla` and
  `test_64bit.kla` build.

Each item marked ✱ below is the first board run of something only reasoned
from the RTL source.

| # | Command | Expect |
|---|---|---|
| B0 | Setup above, on the commit named in the go-ahead message | Both test runs pass. |
| B1 ✱ | `kc -i tests/fixtures/hw_alignment.kla -s /dev/ttyUSB1 -T` | **102/102 PASS.** This one fixture checks every emulator change against the real CPU. On a FAIL, the first wrong value's index maps to a check (a1…k1) in the file header, and the comment beside that check gives the RTL derivation. Report the index, the expected and received values, and the check. |
| B2 ✱ | `kc -L tests/fixtures/all_tests.txt -s /dev/ttyUSB1` | Now 26 entries: 25 with expected values plus test_io (skipped). Expect 25/25 passed. |
| B3 | `kc --emulate-test tests/fixtures/all_tests.txt` | 25/25 (no board). **Every file must get the same result in B2 and B3.** A mismatch is an emulator/hardware difference: report the file and both outputs. |
| B4 ✱ | `kc -i tests/fixtures/v2_hello.kla -s /dev/ttyUSB1 -m`, then the same for `v2_loop.kla` and `v2_math.kla` | Each prints its text (v2_hello: "Hi"). This retests F2. |
| B5 | Make a copy of v2_hello with the TX_BUSY polls removed, then run `kc -i <copy> --emulate` | The emulator reports "UART TX write while busy at PC …". On the board (`-s … -m`) it prints nothing or drops characters. |
| B6 ✱ | `kc -i tests/fixtures/test_64bit.kla -s /dev/ttyUSB1 -T` | PASS (`00000000`). This is the C runtime end to end (libc, malloc/free, v2 UART stubs). |
| B7 ✱ | `kc -i tests/fixtures/test2.kla -s /dev/ttyUSB1 -m` | Prints `Hellow World00000000` and halts. |
| B8 ✱ | `kc -i tests/fixtures/test_strings.kla -s /dev/ttyUSB1 -T`, then the same for `test_txstr.kla` | PASS. These check the byte-string layout and `$TXSTR`. |
| B9 | F1 retest: run a program that ends in HALT, then `kc --gdbserver 3333 --board -i prog.elf` (no `-s`), then a plain `kc -i tests/fixtures/v2_hello.kla -s /dev/ttyUSB1 -m` | The second command gives the "already stopped (HALT …) … load one with -s" error and exit 1. The third runs normally. |
| B10 | Re-run Phase A 3.2 (C ELF on the board) and `klausscc-runtime/debug/run_board_smoke.sh` | Same as Phase A. |
| B11 (optional) | Netload (needs the netboot ROM and network, so ask Graham first): `kc -N prog.elf --ip <board>` | Acked, exit 0. |

## What changed (where to look if something fails)

- **Emulator** (`src/emu/`):
  - `exec.rs`: flags and legality.
  - `mmio.rs`: TX busy (`UART_CYCLES_PER_STORE`, `LOADER_MESSAGE_CYCLES`),
    DELAY time and timers.
  - Unaligned 64-bit accesses go to the aligned doubleword.
  - Every behaviour cites `pipeline_core.sv`, `mem_read_write.sv`,
    `KlaussCPU.sv` or `uart_*.sv` lines; see `doc/EMULATOR_ISA_SEMANTICS.md`.
- **Assembler:**
  - `src/asm/data.rs`: the string layout and inline-data alignment.
  - `src/isa/encode.rs`: the `.S` imm8 ranges.
- **Runtime:** `runtime/uart_stubs.kla` (v2 MMIO, C-callable `_uart_*`) and
  `runtime/crt0.kla`. The fixtures `!include` them.
- **Board I/O and debug** were verified in Phase A. Since then only F1
  (`src/debug/board.rs`) and the USB-only port list
  (`src/link/serial/port.rs`) changed.

## Report format

Send one message to the klausscc session (`klausscc-43`):

```
HW test report (Phase B) — klausscc <sha>, bitstream cf01863 DEBUG_EN=1
Setup: cargo test --features board: <counts>; cargo test: <counts>
| #  | result | note |
| B1 | PASS 102/102 | |
...
Emulator vs hardware mismatches: <file / check: emulator X, board Y>
Failures: <command, full output, exit status, diagnosis, optional patch as diff text>
```

Don't commit to `review-fixes` from the test machine. Send findings and
patches back instead.
