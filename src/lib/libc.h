/* libc.h - Minimal C library for FPGA_CPU_32_DDR_cache
 *
 * All types are 32-bit words. sizeof(int) = sizeof(char) = sizeof(void*) = 1.
 * CHAR_BIT = 32. Strings are word arrays with one character per word.
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
extern void puts(int *s);             /* Print string + newline */
extern void print_str(int *s);        /* Print string (no newline) */
extern void newline(void);            /* Print CR/LF */

/* === Number output === */
extern void print_int(int n);         /* Print signed decimal */
extern void print_unsigned(unsigned n); /* Print unsigned decimal */
extern void print_hex(int val);       /* Print as 8 hex digits (via TXR) */
extern void print_hex_prefix(int val); /* Print 0xNNNNNNNN */

/* === String operations === */
extern int   strlen(int *s);           /* String length */
extern int   strcmp(int *a, int *b);   /* Compare: 0=equal */
extern int  *strcpy(int *dst, int *src); /* Copy string */

/* === Memory operations === */
extern void *memset(int *dst, int c, int n);   /* Fill n words */
extern void *memcpy(int *dst, int *src, int n); /* Copy n words */

/* === Utility === */
extern int   abs(int n);
extern int   min(int a, int b);
extern int   max(int a, int b);
extern void  swap(int *a, int *b);

/* === Heap management (free-list allocator) ===
 *
 * All sizes are in 32-bit WORDS (CHAR_BIT=32; there are no bytes).
 *
 * Memory header layout — four separate 32-bit words, one per address:
 *   mem[0]  heap_start  set by assembler (first word after program)
 *   mem[1]  heap_top    maintained by malloc (high-water mark)
 *   mem[2]  reserved
 *   mem[3]  reserved
 *
 * Each block carries a 3-word overhead header before the user data.
 */
extern void *malloc(int size);              /* allocate 'size' words          */
extern void  free(void *ptr);               /* release allocation             */
extern void *calloc(int nmemb, int size);   /* allocate + zero-fill           */
extern void *realloc(void *ptr, int size);  /* resize (never shrinks)         */
extern int   heap_words_used(void);         /* words live in allocated blocks */
extern int   heap_words_free(void);         /* words sitting in free blocks   */

/* === Low-level UART (also available directly) === */
extern void _uart_tx_hex(int val);     /* TXR wrapper */
extern void _uart_newline(void);       /* NEWLINE wrapper */
extern void _uart_tx_char(int ch);     /* Single char via TXCHARMEMR */

#endif /* LIBC_H */
