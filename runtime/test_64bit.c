/* test_64bit.c
 *
 * Comprehensive test suite for the 64-bit FPGA CPU.
 * sizeof(int) = sizeof(void*) = 8 bytes on this target.
 *
 * Tests:
 *   T1  64-bit constant shifts (shift > 31, up to 63)
 *   T2  64-bit multiplication (product crosses 32-bit boundary)
 *   T3  64-bit addition crosses 0x80000000 without overflow
 *   T4  Bitwise NOT (NOTR) operates on all 64 bits
 *   T5  Arithmetic right shift of negative value (SHRA)
 *   T6  Logical right shift of unsigned value (SHRL)
 *   T7  Signed/unsigned comparisons with large 64-bit values
 *   T8  Recursion: factorial (10! and 12!)
 *   T9  Recursion: fibonacci (fib(10) and fib(15))
 *   T10 Overflow arguments: sum5() and sum6() (5th/6th arg on stack)
 *   T11 Char array byte access (MEMGET8 / MEMSET8)
 *   T12 String operations: strlen, strcmp, strcpy
 *   T13 Memory operations: memset, memcpy
 *   T14 Global variable read/write via function calls
 *   T15 Pointer arithmetic: p+1 advances sizeof(int)=8 bytes
 *   T16 Heap: malloc, calloc, realloc, free, coalesce
 *
 * Compile: build/rcc -target=klacpu tests/test_64bit.c
 * Link with: lib/libc.kla lib/uart_stubs.kla
 */

#include "lib/libc.h"

/* ----------------------------------------------------------------
 * Globals: pass/fail counters and a variable for the global test
 * ---------------------------------------------------------------- */
int g_pass;
int g_fail;
int g_val;

/* ----------------------------------------------------------------
 * check() — print test name + PASS or FAIL, update counters
 * ---------------------------------------------------------------- */
void check(char *name, int ok) {
    print_str(name);
    if (ok) {
        print_str("PASS");
        g_pass = g_pass + 1;
    } else {
        print_str("FAIL");
        g_fail = g_fail + 1;
    }
    newline();
}

/* ----------------------------------------------------------------
 * check_eq() — compare actual vs expected; on failure print both
 *              as 16-digit hex so bit patterns are visible.
 * ---------------------------------------------------------------- */
void check_eq(char *name, int actual, int expected) {
    print_str(name);
    if (actual == expected) {
        print_str("PASS");
        g_pass = g_pass + 1;
    } else {
        print_str("FAIL  got=");
        print_hex_prefix(actual);
        print_str(" exp=");
        print_hex_prefix(expected);
        g_fail = g_fail + 1;
    }
    newline();
}

/* ----------------------------------------------------------------
 * Recursive helpers
 * ---------------------------------------------------------------- */
int factorial(int n) {
    if (n <= 1) return 1;
    return n * factorial(n - 1);
}

int fib(int n) {
    if (n <= 0) return 0;
    if (n == 1) return 1;
    return fib(n - 1) + fib(n - 2);
}

/* ----------------------------------------------------------------
 * Functions with more than 4 args (5th+ arg passed on stack)
 * ---------------------------------------------------------------- */
int sum5(int a, int b, int c, int d, int e) {
    return a + b + c + d + e;
}

int sum6(int a, int b, int c, int d, int e, int f) {
    return a + b + c + d + e + f;
}

/* ----------------------------------------------------------------
 * Global access helpers (for T14)
 * ---------------------------------------------------------------- */
void set_global(int v) { g_val = v; }
int  get_global(void)  { return g_val; }

/* ================================================================
 * main
 * ================================================================ */
int main(void) {
    int a;
    int b;
    int c;
    int ok;
    char buf[32];

    g_pass = 0;
    g_fail = 0;

    print_str("=== 64-bit CPU Test Suite ===");
    newline();

    /* ============================================================
     * System info
     * ============================================================ */
    {
        int *mem;
        int sz;
        int arr2[2];
        mem = (int *)0;
        print_str("heap_start=");
        print_hex_prefix(mem[0]);
        /* sizeof(int) inferred from pointer stride: &arr2[1] - &arr2[0] */
        sz = (int)((char *)(&arr2[1]) - (char *)(&arr2[0]));
        print_str("  sizeof(int)=");
        print_int(sz);
        newline();
    }
    newline();

    /* ============================================================
     * T1: 64-bit constant shift (shift amount > 31)
     * ============================================================ */
    a = 1;
    a = a << 40;        /* 2^40 = 0x10000000000 */
    b = a >> 20;        /* 2^20 = 1048576        */
    check("T1  shift>31:     ", b == 1048576);

    /* ============================================================
     * T2: 64-bit multiplication — product exceeds 32-bit range
     * ============================================================ */
    a = 0x100000;
    b = 0x100000;
    c = a * b;          /* 2^40 */
    c = c >> 20;
    check("T2  mul64:        ", c == 1048576);

    /* ============================================================
     * T3: 64-bit addition crosses INT32_MAX boundary
     * ============================================================ */
    a = 0x7FFFFFFF;
    b = a + 1;          /* 0x80000000 — positive in 64-bit */
    ok = (b > 0) && (b == 0x80000000);
    a  = b + b;         /* 0x100000000 = 2^32 */
    ok = ok && (a > 0) && (a > 0x7FFFFFFF);
    check("T3  add64 no ovf: ", ok);

    /* ============================================================
     * T4: Bitwise NOT — NOTR must flip all 64 bits
     *
     * T4a: ~0 must produce all 64 bits set (= -1 signed).
     *      If NOTR only flips 32 bits, result is 0x00000000FFFFFFFF ≠ -1.
     * T4b: ~~0 must recover 0.
     * T4c: low 32 bits of ~0x5A5A5A5A must be 0xA5A5A5A5.
     * T4d: arithmetic right-shift of ~0x5A5A5A5A by 32 must be -1
     *      (all 1s) — only true if the high 32 bits were all 1 (64-bit NOT).
     * ============================================================ */
    {
        int b0, a1, blow, bhigh;
        a     = 0;
        b0    = ~a;             /* ~0: should be 0xFFFFFFFFFFFFFFFF = -1 */
        a1    = ~b0;            /* ~~0: should be 0 */
        a     = 0x5A5A5A5A;
        b     = ~a;
        blow  = b & 0xFFFFFFFF; /* low 32 bits of ~0x5A5A5A5A */
        bhigh = b >> 32;        /* high 32 bits: all 1 if NOTR is 64-bit */
        check_eq("T4a ~0==-1:       ", b0,    -1);
        check_eq("T4b ~~0==0:       ", a1,    0);
        check_eq("T4c ~5A low32:    ", blow,  0xA5A5A5A5);
        check_eq("T4d ~5A >>32==-1: ", bhigh, -1);
    }

    /* ============================================================
     * T5: Arithmetic right shift of negative value (SHRA)
     *
     * T5a: -1 >> 63 — shift count > 31 requires 64-bit SHRA.
     * T5b: -1 >> 1  — must sign-extend (not logical shift).
     * T5c: -256 >> 4 = -16.
     * ============================================================ */
    {
        int neg1;
        int neg256;
        int r1, r2, r3;
        neg1   = -1;
        neg256 = -256;
        r1 = neg1   >> 63;
        r2 = neg1   >> 1;
        r3 = neg256 >> 4;
        check_eq("T5a -1>>63==-1:   ", r1, -1);
        check_eq("T5b -1>>1==-1:    ", r2, -1);
        check_eq("T5c -256>>4==-16: ", r3, -16);
    }

    /* ============================================================
     * T6: Logical right shift of unsigned value (SHRL)
     *
     * T6a: (unsigned)(-1) >> 63 must give 1.
     *      If SHRV is 32-bit only, 0xFFFFFFFF >> 63 = 0 (count >= width).
     * T6b: (unsigned)(-1) >> 1 must be positive as signed int
     *      (MSB cleared by logical shift — not arithmetic).
     * ============================================================ */
    {
        unsigned ua;
        ua = -1;            /* 0xFFFFFFFFFFFFFFFF */
        check_eq("T6a >>63==1:      ", (int)(ua >> 63), 1);
        check_eq("T6b >>1 positive: ", ((int)(ua >> 1) > 0), 1);
    }

    /* ============================================================
     * T7: Comparisons with large 64-bit values
     *
     * T7a/b: 2^40 stored in 'a' must compare as a large positive.
     *        If registers are 32-bit, 2^40 mod 2^32 = 0, so both fail.
     * T7c: -1 < 2^40 (signed comparison).
     * T7d: -1 < 0.
     * T7e: unsigned max > 2^40.
     * ============================================================ */
    {
        int large;
        int neg;
        unsigned ua;
        unsigned ub;
        large = 1;
        large = large << 40;        /* 2^40 = 0x10000000000 */
        neg   = -1;
        ua    = (unsigned)(-1);
        ub    = (unsigned)large;
        check_eq("T7a 2^40>0:       ", (large > 0), 1);
        check_eq("T7b 2^40>0x7F..:  ", (large > 0x7FFFFFFF), 1);
        check_eq("T7c -1<2^40:      ", (neg < large), 1);
        check_eq("T7d -1<0:         ", (neg < 0), 1);
        check_eq("T7e MaxU>2^40:    ", (int)(ua > ub), 1);
    }

    /* ============================================================
     * T8: Recursion — factorial
     * ============================================================ */
    ok = (factorial(10) == 3628800);
    ok = ok && (factorial(12) == 479001600);
    check("T8  factorial:    ", ok);

    /* ============================================================
     * T9: Recursion — Fibonacci
     * ============================================================ */
    ok = (fib(10) == 55);
    ok = ok && (fib(15) == 610);
    check("T9  fibonacci:    ", ok);

    /* ============================================================
     * T10: Overflow arguments (5th and 6th arg passed on stack)
     * ============================================================ */
    check_eq("T10a sum5(1..5):  ", sum5(1, 2, 3, 4, 5),           15);
    check_eq("T10b sum5(10..50):", sum5(10, 20, 30, 40, 50),       150);
    check_eq("T10c sum6(10..60):", sum6(10, 20, 30, 40, 50, 60),   210);

    /* ============================================================
     * T11: Char array — byte-granular MEMGET8 / MEMSET8
     * ============================================================ */
    buf[0] = 72;    /* 'H' */
    buf[1] = 101;   /* 'e' */
    buf[2] = 121;   /* 'y' */
    buf[3] = 33;    /* '!' */
    buf[4] = 0;
    ok = (buf[0] == 72) && (buf[1] == 101) &&
         (buf[2] == 121) && (buf[3] == 33) && (buf[4] == 0);
    ok = ok && (strlen(buf) == 4);
    check("T11 char[]:       ", ok);

    /* ============================================================
     * T12: String operations — strlen, strcmp, strcpy
     * ============================================================ */
    strcpy(buf, "hello");
    ok = (strlen(buf) == 5);
    ok = ok && (strcmp(buf, "hello") == 0);
    ok = ok && (strcmp(buf, "hellp") < 0);
    ok = ok && (strcmp(buf, "helln") > 0);
    ok = ok && (strcmp(buf, "hell")  > 0);
    ok = ok && (strcmp(buf, "hellow") < 0);
    check("T12 strings:      ", ok);

    /* ============================================================
     * T13: memset / memcpy
     * ============================================================ */
    {
        char src[24];
        char dst[24];
        memset(src, 0xAB, 8);
        memset(src + 8, 0x00, 8);
        memset(src + 16, 0x55, 8);
        ok = ((src[0] & 0xFF) == 0xAB) && ((src[7]  & 0xFF) == 0xAB);
        ok = ok && (src[8] == 0)  && (src[15] == 0);
        ok = ok && ((src[16] & 0xFF) == 0x55) && ((src[23] & 0xFF) == 0x55);
        memcpy(dst, src, 24);
        ok = ok && ((dst[0] & 0xFF) == 0xAB);
        ok = ok && (dst[8] == 0);
        ok = ok && ((dst[16] & 0xFF) == 0x55);
        check("T13 memset/cpy:   ", ok);
    }

    /* ============================================================
     * T14: Global variable read/write via function calls
     * ============================================================ */
    set_global(0xDEADBEEF);
    ok = (get_global() == 0xDEADBEEF);
    set_global(0);
    ok = ok && (get_global() == 0);
    set_global(-42);
    ok = ok && (get_global() == -42);
    check("T14 globals:      ", ok);

    /* ============================================================
     * T15: Pointer arithmetic — sizeof(int)=8, so p+1 = +8 bytes
     * ============================================================ */
    {
        int arr[4];
        int *p;
        int dist;
        arr[0] = 100;
        arr[1] = 200;
        arr[2] = 300;
        arr[3] = 400;
        p  = arr;
        ok = (*p == 100);
        p  = p + 1;
        ok = ok && (*p == 200);
        p  = p + 1;
        ok = ok && (*p == 300);
        p  = p + 1;
        ok = ok && (*p == 400);
        dist = (int)((char *)(&arr[3]) - (char *)(&arr[0]));
        ok = ok && (dist == 24);        /* 3 * sizeof(int) = 3 * 8 */
        check("T15 ptr arith:    ", ok);
    }

    /* ============================================================
     * T16a: Heap — malloc write/read, and accounting
     * ============================================================ */
    {
        int *hp1;
        int *hp2;
        int *hp3;
        int hs;
        int ht;
        int *mem;

        mem = (int *)0;
        hs  = mem[0];   /* heap_start: 8-byte read at address 0 */

        hp1    = (int *)malloc(16);     /* 2 eight-byte words */
        hp1[0] = 0x1111;
        hp1[1] = 0x2222;
        ok = (hp1[0] == 0x1111) && (hp1[1] == 0x2222);
        ht = heap_get_top();
        ok = ok && ((ht - hs) == 40);  /* 5 words * 8 bytes  */
        ok = ok && (heap_words_used() == 2);
        check("T16a heap alloc:  ", ok);

        /* T16b: calloc zero-fill */
        hp2 = (int *)calloc(3, 8);     /* 3 eight-byte words, zeroed */
        ok  = (hp2[0] == 0) && (hp2[1] == 0) && (hp2[2] == 0);
        check("T16b calloc:      ", ok);

        /* T16c: realloc — grow allocation, verify old data survives.
         * Each element checked individually to pinpoint any failure. */
        hp3    = (int *)malloc(8);      /* 1 word */
        hp3[0] = 0x5A5A5A5A;
        hp3    = (int *)realloc(hp3, 24);   /* grow to 3 words */
        check_eq("T16c-0 keep[0]:   ", hp3[0], 0x5A5A5A5A);
        hp3[1] = 0xBEEFBEEF;
        hp3[2] = 0xCAFECAFE;
        check_eq("T16c-1 new[1]:    ", hp3[1], 0xBEEFBEEF);
        check_eq("T16c-2 new[2]:    ", hp3[2], 0xCAFECAFE);

        /* T16d: free all blocks, verify full coalescing */
        free(hp1);
        free(hp2);
        free(hp3);
        ok = (heap_words_used() == 0);
        check("T16d coalesce:    ", ok);
    }

    /* ============================================================
     * Summary
     * ============================================================ */
    newline();
    print_str("Results: ");
    print_int(g_pass);
    print_str(" pass, ");
    print_int(g_fail);
    print_str(" fail");
    newline();

    return g_fail;  /* 0 = all passed */
}
