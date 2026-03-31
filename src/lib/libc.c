/* libc.c - Minimal C library for FPGA_CPU_32_DDR_cache
 *
 * All types are 32-bit words (CHAR_BIT=32, sizeof(int)=1).
 * No floating point. No file I/O. Output via UART only.
 *
 * Compile with: build/rcc -target=klacpu lib/libc.c > lib/libc.asm
 */

/* === Low-level UART stubs (implemented in uart_stubs.kla) === */
extern void _uart_tx_hex(int val);
extern void _uart_newline(void);
extern void _uart_tx_char(int ch);

/* Forward declarations */
void print_unsigned(unsigned n);
void _print_neg(int neg, int orig);
void print_str(int *s);

/* =============================================================
 * Character output
 * ============================================================= */

void putchar(int ch) {
    _uart_tx_char(ch);
}

void puts(int *s) {
    while (*s != 0) {
        putchar(*s);
        s = s + 1;
    }
    putchar(10);  /* newline */
}

/* Print a string without trailing newline */
void print_str(int *s) {
    while (*s != 0) {
        putchar(*s);
        s = s + 1;
    }
}

/* =============================================================
 * Number output
 * ============================================================= */

/* Print unsigned integer in decimal */
void print_unsigned(unsigned n) {
    /* Recursive: print higher digits first */
    if (n >= 10)
        print_unsigned(n / 10);
    putchar((n % 10) + 48);  /* 48 = '0' */
}

/* Print signed integer in decimal */
void print_int(int n) {
    if (n < 0) {
        putchar(45);  /* '-' */
        /* Print digits from most significant: use print_int_helper */
        _print_neg(-n, n);
        return;
    }
    print_unsigned(n);
}

/* Helper for negative numbers: if -n overflowed (n==MIN_INT), 
   neg will equal n. We detect this and handle specially. */
void _print_neg(int neg, int orig) {
    if (neg < 0) {
        /* Overflow: orig was MIN_INT. Print "2147483648" literally */
        putchar(50); putchar(49); putchar(52); putchar(55);
        putchar(52); putchar(56); putchar(51); putchar(54);
        putchar(52); putchar(56);
    } else {
        print_unsigned(neg);
    }
}

/* Print 32-bit value as 8 hex digits */
void print_hex(int val) {
    _uart_tx_hex(val);
}

/* Print hex with 0x prefix */
void print_hex_prefix(int val) {
    putchar(48);   /* '0' */
    putchar(120);  /* 'x' */
    _uart_tx_hex(val);
}

/* Print newline (CR/LF) */
void newline(void) {
    _uart_newline();
}

/* Printf-lite: supports %d, %u, %x, %s, %c, %% only
 * Uses varargs-style stack access.
 * NOTE: This is a simplified version - format string and all args
 * must be passed individually since we don't have real varargs.
 */

/* =============================================================
 * String operations
 * ============================================================= */

/* String length (word-sized characters) */
int strlen(int *s) {
    int len;
    len = 0;
    while (*s != 0) {
        len = len + 1;
        s = s + 1;
    }
    return len;
}

/* String compare: returns 0 if equal, <0 or >0 otherwise */
int strcmp(int *a, int *b) {
    while (*a != 0 && *a == *b) {
        a = a + 1;
        b = b + 1;
    }
    return *a - *b;
}

/* String copy: copies src to dst, returns dst */
int *strcpy(int *dst, int *src) {
    int *ret;
    ret = dst;
    while (*src != 0) {
        *dst = *src;
        dst = dst + 1;
        src = src + 1;
    }
    *dst = 0;
    return ret;
}

/* =============================================================
 * Memory operations
 * ============================================================= */

/* Set n words to value c */
void *memset(int *dst, int c, int n) {
    int *p;
    p = dst;
    while (n > 0) {
        *p = c;
        p = p + 1;
        n = n - 1;
    }
    return dst;
}

/* Copy n words from src to dst */
void *memcpy(int *dst, int *src, int n) {
    int *d;
    int *s;
    d = dst;
    s = src;
    while (n > 0) {
        *d = *s;
        d = d + 1;
        s = s + 1;
        n = n - 1;
    }
    return dst;
}

/* =============================================================
 * Utility functions
 * ============================================================= */

/* Absolute value */
int abs(int n) {
    if (n < 0) return -n;
    return n;
}

/* Min / Max */
int min(int a, int b) {
    if (a < b) return a;
    return b;
}

int max(int a, int b) {
    if (a > b) return a;
    return b;
}

/* Swap two values via pointers */
void swap(int *a, int *b) {
    int t;
    t = *a;
    *a = *b;
    *b = t;
}
