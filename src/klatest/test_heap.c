/* test_heap.c - Exercise the free-list heap allocator in libc.c
 *
 * Compile: build/rcc -target=klacpu tests/test_heap.c > tests/test_heap.kla
 *
 * All malloc/calloc/realloc sizes are in BYTES (sizeof(int)=4).
 * Internally the allocator rounds up to 4-byte words.
 * heap_words_used() and heap_words_free() report in 4-byte words.
 *
 * Tests:
 *   T1  malloc read/write  — allocate 5 ints (20 bytes), write and read back
 *   T2  heap accounting    — heap_top advances by 32 bytes (8 words: 3 hdr + 5 data)
 *   T3  calloc zero-fill   — calloc(4, sizeof(int)) returns 4 zeroed ints
 *   T4  free + reuse       — free middle block, reallocate smaller (split check)
 *   T5  coalescing         — free all blocks; must merge back to one free region
 *   T6  realloc            — grow allocation; old data survives, new words accessible
 *
 * Expected output (XX = program-size-dependent hex address):
 *   === Heap Test ===
 *   heap_start=0x000000XX
 *   T1 malloc R/W:  PASS
 *   T2 heap acctg:  PASS  advance=32 used=5
 *   T3 calloc zero: PASS
 *   T4 free/reuse:  PASS  used=21 free=1
 *   T5 coalesce:    PASS  used=0 free=34
 *   T6 realloc:     PASS
 *   Done.
 */

#include "libc.h"

int main() {
    int *mem;   /* pointer to byte 0: heap header lives at byte addresses 0-15 */
    int *p1;
    int *p2;
    int *p3;
    int *p4;
    int *p5;
    int  ok;
    int  hs;    /* heap_start snapshot (byte address) */
    int  ht;    /* heap_top snapshot   (byte address) */

    mem = (int *)0;

    /* --- Banner and heap_start (informational) --- */
    print_str("=== Heap Test ===");
    newline();
    print_str("heap_start=");
    print_hex_prefix(mem[0]);
    newline();

    /* ==========================================================
     * T1: malloc read/write
     *   Allocate 5 ints (20 bytes).  Write distinct values, read back.
     * ========================================================== */
    p1 = (int *)malloc(20);
    p1[0] = 11;
    p1[1] = 22;
    p1[2] = 33;
    p1[3] = 44;
    p1[4] = 55;
    ok = (p1[0] == 11 && p1[1] == 22 && p1[2] == 33 &&
          p1[3] == 44 && p1[4] == 55);
    print_str("T1 malloc R/W:  ");
    if (ok) print_str("PASS"); else print_str("FAIL");
    newline();

    /* ==========================================================
     * T2: heap accounting
     *   After malloc(20), heap_top must have advanced by exactly
     *   32 bytes (8 words * 4 bytes/word) from heap_start:
     *     3-word block header + 5 data words = 8 words = 32 bytes.
     *   heap_words_used() must return 5.
     * ========================================================== */
    hs  = mem[0];
    ht  = heap_get_top();
    ok  = ((ht - hs) == 32) && (heap_words_used() == 5);
    print_str("T2 heap acctg:  ");
    if (ok) print_str("PASS"); else print_str("FAIL");
    print_str("  advance=");  print_int(ht - hs);
    print_str(" used=");      print_int(heap_words_used());
    newline();

    /* ==========================================================
     * T3: calloc zero-initialisation
     *   calloc(4, 4) allocates 16 bytes = 4 ints, all must read as 0.
     * ========================================================== */
    p2 = (int *)calloc(4, 4);
    ok  = (p2[0] == 0 && p2[1] == 0 && p2[2] == 0 && p2[3] == 0);
    print_str("T3 calloc zero: ");
    if (ok) print_str("PASS"); else print_str("FAIL");
    newline();

    /* ==========================================================
     * T4: free a block, then reallocate smaller (split test)
     *
     *   Allocate p3=32 bytes (8 words), p4=32 bytes (8 words).
     *   Free p3 (8-word free block on the list).
     *   Allocate p5=16 bytes (4 words):
     *     split condition: 8 >= 4 + HDRSIZE(3) + MIN_SPLIT(1) = 8  YES
     *     -> p5 gets 4 words; 1-word remnant stays free.
     *
     *   Expected: used = 5+4+4+8 = 21,  free = 1
     * ========================================================== */
    p3 = (int *)malloc(32);
    p4 = (int *)malloc(32);
    free(p3);
    p5    = (int *)malloc(16);
    p5[0] = 99;                  /* write sentinel to confirm it's writable */
    ok    = (p5[0] == 99) && (heap_words_used() == 21) && (heap_words_free() == 1);
    print_str("T4 free/reuse:  ");
    if (ok) print_str("PASS"); else print_str("FAIL");
    print_str(" used="); print_int(heap_words_used());
    print_str(" free="); print_int(heap_words_free());
    newline();

    /* ==========================================================
     * T5: coalescing — free everything, check the free list
     *     collapses back to a single contiguous region.
     *
     *   free(p4): 1-word remnant + p4(8)  merge -> 12-word block
     *   free(p5): p5(4)  + 12-word block  merge -> 19-word block
     *   free(p2): p2(4)  + 19-word block  merge -> 26-word block
     *   free(p1): p1(5)  + 26-word block  merge -> 34-word block
     *
     *   Total heap = 3+5 + 3+4 + 3+4 + 3+1 + 3+8 = 37 words
     *   One remaining header (3 words) -> free data = 37-3 = 34
     *
     *   Expected: used=0, free=34
     * ========================================================== */
    free(p4);
    free(p5);
    free(p2);
    free(p1);
    ok = (heap_words_used() == 0) && (heap_words_free() == 34);
    print_str("T5 coalesce:    ");
    if (ok) print_str("PASS"); else print_str("FAIL");
    print_str(" used="); print_int(heap_words_used());
    print_str(" free="); print_int(heap_words_free());
    newline();

    /* ==========================================================
     * T6: realloc — grow an allocation; original data must survive
     *
     *   malloc(12) = 3 ints, write 7,8,9.
     *   realloc to 24 bytes (6 ints): malloc(24)+memcpy+free old.
     *   First 3 ints must still hold 7,8,9.
     *   New ints 3-5 are writable.
     * ========================================================== */
    p1 = (int *)malloc(12);
    p1[0] = 7;
    p1[1] = 8;
    p1[2] = 9;
    p1 = (int *)realloc(p1, 24);
    p1[3] = 10;
    p1[4] = 11;
    p1[5] = 12;
    ok = (p1[0] == 7  && p1[1] == 8  && p1[2] == 9 &&
          p1[3] == 10 && p1[4] == 11 && p1[5] == 12);
    print_str("T6 realloc:     ");
    if (ok) print_str("PASS"); else print_str("FAIL");
    newline();

    print_str("Done.");
    newline();
    return 0;
}
