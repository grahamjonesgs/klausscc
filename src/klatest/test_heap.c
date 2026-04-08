/* test_heap.c - Exercise the free-list heap allocator in libc.c
 *
 * Compile: build/rcc -target=klacpu tests/test_heap.c > tests/test_heap.kla
 *
 * Tests (all sizes are 32-bit WORDS, not bytes):
 *   T1  malloc read/write — allocate 5 words, write and read back
 *   T2  heap accounting   — heap_top advanced by 8 (HDRSIZE+5), used=5
 *   T3  calloc zero-fill  — calloc(4,1) returns 4 zeroed words
 *   T4  free + reuse      — free middle block, reallocate smaller (split check)
 *   T5  coalescing        — free all blocks; must merge back to one free region
 *   T6  realloc           — grow allocation; old data survives, new words accessible
 *
 * Expected output (XX = program-size-dependent hex address):
 *   === Heap Test ===
 *   heap_start=0x000000XX
 *   T1 malloc R/W:  PASS
 *   T2 heap acctg:  PASS  advance=8 used=5
 *   T3 calloc zero: PASS
 *   T4 free/reuse:  PASS  used=21 free=1
 *   T5 coalesce:    PASS  used=0 free=34
 *   T6 realloc:     PASS
 *   Done.
 */

extern void  putchar(int ch);
extern void  print_str(int *s);
extern void  print_int(int n);
extern void  print_hex_prefix(int val);
extern void  newline(void);
extern void *malloc(int size);
extern void  free(void *ptr);
extern void *calloc(int nmemb, int size);
extern void *realloc(void *ptr, int new_size);
extern int   heap_words_used(void);
extern int   heap_words_free(void);

int main() {
    int *mem;   /* pointer to word 0: heap header lives at addresses 0-3 */
    int *p1;
    int *p2;
    int *p3;
    int *p4;
    int *p5;
    int  ok;
    int  hs;    /* heap_start snapshot */
    int  ht;    /* heap_top snapshot   */

    mem = (int *)0;

    /* --- Banner and heap_start (informational) --- */
    print_str((int *)"=== Heap Test ===");
    newline();
    print_str((int *)"heap_start=");
    print_hex_prefix(mem[0]);
    newline();

    /* ==========================================================
     * T1: malloc read/write
     *   Allocate 5 words.  Write distinct values, read them back.
     * ========================================================== */
    p1 = (int *)malloc(5);
    p1[0] = 11;
    p1[1] = 22;
    p1[2] = 33;
    p1[3] = 44;
    p1[4] = 55;
    ok = (p1[0] == 11 && p1[1] == 22 && p1[2] == 33 &&
          p1[3] == 44 && p1[4] == 55);
    print_str((int *)"T1 malloc R/W:  ");
    if (ok) print_str((int *)"PASS"); else print_str((int *)"FAIL");
    newline();

    /* ==========================================================
     * T2: heap accounting
     *   After malloc(5), mem[1] (heap_top) must have advanced by
     *   exactly 8 words from mem[0] (heap_start):
     *     3-word block header + 5 data words = 8.
     *   heap_words_used() must return 5.
     * ========================================================== */
    hs  = mem[0];
    ht  = mem[1];
    ok  = ((ht - hs) == 8) && (heap_words_used() == 5);
    print_str((int *)"T2 heap acctg:  ");
    if (ok) print_str((int *)"PASS"); else print_str((int *)"FAIL");
    print_str((int *)"  advance=");  print_int(ht - hs);
    print_str((int *)" used=");      print_int(heap_words_used());
    newline();

    /* ==========================================================
     * T3: calloc zero-initialisation
     *   calloc(4, 1) allocates 4 words, all must read as 0.
     * ========================================================== */
    p2 = (int *)calloc(4, 1);
    ok  = (p2[0] == 0 && p2[1] == 0 && p2[2] == 0 && p2[3] == 0);
    print_str((int *)"T3 calloc zero: ");
    if (ok) print_str((int *)"PASS"); else print_str((int *)"FAIL");
    newline();

    /* ==========================================================
     * T4: free a block, then reallocate smaller (split test)
     *
     *   Allocate p3=8 words, p4=8 words.
     *   Free p3 (8-word free block on the list).
     *   Allocate p5=4 words:
     *     split condition: 8 >= 4 + HDRSIZE(3) + MIN_SPLIT(1) = 8  YES
     *     -> p5 gets 4 words; 1-word remnant stays free.
     *
     *   Heap layout after this step (H = heap_start):
     *     H+0:  [5,alloc] p1
     *     H+8:  [4,alloc] p2 (from calloc)
     *     H+15: [4,alloc] p5  (reused from p3's slot, split)
     *     H+22: [1,free ]  split remnant
     *     H+26: [8,alloc] p4
     *
     *   Expected: used = 5+4+4+8 = 21,  free = 1
     * ========================================================== */
    p3 = (int *)malloc(8);
    p4 = (int *)malloc(8);
    free(p3);
    p5    = (int *)malloc(4);
    p5[0] = 99;                 /* write sentinel to confirm it's writable */
    ok    = (p5[0] == 99) && (heap_words_used() == 21) && (heap_words_free() == 1);
    print_str((int *)"T4 free/reuse:  ");
    if (ok) print_str((int *)"PASS"); else print_str((int *)"FAIL");
    print_str((int *)" used="); print_int(heap_words_used());
    print_str((int *)" free="); print_int(heap_words_free());
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
    print_str((int *)"T5 coalesce:    ");
    if (ok) print_str((int *)"PASS"); else print_str((int *)"FAIL");
    print_str((int *)" used="); print_int(heap_words_used());
    print_str((int *)" free="); print_int(heap_words_free());
    newline();

    /* ==========================================================
     * T6: realloc — grow an allocation; original data must survive
     *
     *   malloc(3), write 7,8,9.
     *   realloc to 6: implementation does malloc(6)+memcpy+free old.
     *   First 3 words must still hold 7,8,9.
     *   New words 3-5 are writable.
     * ========================================================== */
    p1 = (int *)malloc(3);
    p1[0] = 7;
    p1[1] = 8;
    p1[2] = 9;
    p1 = (int *)realloc(p1, 6);
    p1[3] = 10;
    p1[4] = 11;
    p1[5] = 12;
    ok = (p1[0] == 7  && p1[1] == 8  && p1[2] == 9 &&
          p1[3] == 10 && p1[4] == 11 && p1[5] == 12);
    print_str((int *)"T6 realloc:     ");
    if (ok) print_str((int *)"PASS"); else print_str((int *)"FAIL");
    newline();

    print_str((int *)"Done.");
    newline();
    return 0;
}
