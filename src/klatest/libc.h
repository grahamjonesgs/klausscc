/* libc.h - Minimal C library for FPGA_CPU_32_DDR_cache
 *
 * Byte-addressed CPU. sizeof(char)=1, sizeof(int)=sizeof(void*)=4.
 * CHAR_BIT=8. Strings are standard null-terminated byte arrays.
 *
 * Usage:
 *   #include "libc.h"      (if using a preprocessor)
 *   or paste the externs you need into your .c file
 *
 * Link with: precompiled lib/libc.kla + lib/uart_stubs.kla
 */

#ifndef LIBC_H
#define LIBC_H

/* === Character and string output === */
extern void putchar(int ch);           /* Print single ASCII character */
extern void puts(char *s);             /* Print string + newline */
extern void print_str(char *s);        /* Print string (no newline) */
extern void newline(void);             /* Print CR/LF */

/* === Number output === */
extern void print_int(int n);         /* Print signed decimal */
extern void print_unsigned(unsigned n); /* Print unsigned decimal */
extern void print_hex(int val);       /* Print as 8 hex digits (via TXR) */
extern void print_hex_prefix(int val); /* Print 0xNNNNNNNN */

/* === String operations === */
extern int    strlen(char *s);              /* String length */
extern int    strcmp(char *a, char *b);     /* Compare: 0=equal */
extern char  *strcpy(char *dst, char *src); /* Copy string */

/* === Memory operations === */
extern void *memset(char *dst, int c, int n);    /* Fill n bytes */
extern void *memcpy(char *dst, char *src, int n); /* Copy n bytes */

/* === Utility === */
extern int   abs(int n);
extern int   min(int a, int b);
extern int   max(int a, int b);
extern void  swap(int *a, int *b);

/* === Heap management (free-list allocator) ===
 *
 * All sizes are in BYTES (standard C convention, CHAR_BIT=8).
 *
 * Memory header layout — four 32-bit words at byte addresses 0-15:
 *   [0]  heap_start  set by assembler (byte address of first heap word, read-only)
 *   [4]  (unused — heap_top is tracked in an internal static variable)
 *   [8]  reserved
 *   [12] reserved
 *
 * Each block carries a 3-word (12-byte) header before the user data.
 * malloc() internally rounds size up to the nearest word (4 bytes).
 */
extern void *malloc(int size);              /* allocate 'size' bytes          */
extern void  free(void *ptr);               /* release allocation             */
extern void *calloc(int nmemb, int size);   /* allocate + zero-fill           */
extern void *realloc(void *ptr, int size);  /* resize (never shrinks)         */
extern int   heap_words_used(void);         /* 4-byte words in live blocks    */
extern int   heap_words_free(void);         /* 4-byte words in free blocks    */
extern int   heap_get_top(void);            /* current heap_top as byte addr  */

/* === Low-level UART (also available directly) === */
extern void _uart_tx_hex(int val);     /* TXR wrapper */
extern void _uart_newline(void);       /* NEWLINE wrapper */
extern void _uart_tx_char(int ch);     /* Single char via TXCHARMEMR */

#endif /* LIBC_H */
