/* libc.c - Minimal C library for FPGA_CPU_32_DDR_cache
 *
 * Byte-addressed CPU. sizeof(char)=1, sizeof(int)=sizeof(void*)=4.
 * CHAR_BIT=8. No floating point. No file I/O. Output via UART only.
 *
 * Compile with: build/rcc -target=klacpu lib/libc.c > lib/libc.asm
 */

/* === Low-level UART stubs (implemented in uart_stubs.kla) === */
extern void _uart_tx_hex(int val);
extern void _uart_newline(void);
extern void _uart_tx_char(int ch);
extern int  _uart_rx_char(void);
extern int  _uart_rx_char_nb(void);

/* Forward declarations */
void print_unsigned(unsigned n);
void _print_neg(int neg, int orig);
void print_str(char *s);

/* =============================================================
 * Character output
 * ============================================================= */

void putchar(int ch) {
    _uart_tx_char(ch);
}

/* Blocking receive: waits until a byte arrives, returns it */
int getchar(void) {
    return _uart_rx_char();
}

/* Non-blocking receive: returns received byte, or -1 if FIFO was empty.
 * The -1 sentinel is produced by the stub (flag test + NOTR); no CPU
 * flag inspection is needed here. */
int getchar_nb(void) {
    return _uart_rx_char_nb();
}

void puts(char *s) {
    while (*s != 0) {
        putchar(*s);
        s = s + 1;
    }
    putchar(10);  /* newline */
}

/* Print a string without trailing newline */
void print_str(char *s) {
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

/* String length (byte characters) */
int strlen(char *s) {
    int len;
    len = 0;
    while (*s != 0) {
        len = len + 1;
        s = s + 1;
    }
    return len;
}

/* String compare: returns 0 if equal, <0 or >0 otherwise */
int strcmp(char *a, char *b) {
    while (*a != 0 && *a == *b) {
        a = a + 1;
        b = b + 1;
    }
    return *a - *b;
}

/* String copy: copies src to dst, returns dst */
char *strcpy(char *dst, char *src) {
    char *ret;
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

/* Set n bytes to value c */
void *memset(char *dst, int c, int n) {
    char *p;
    p = dst;
    while (n > 0) {
        *p = c;
        p = p + 1;
        n = n - 1;
    }
    return dst;
}

/* Copy n bytes from src to dst */
void *memcpy(char *dst, char *src, int n) {
    char *d;
    char *s;
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

/* =============================================================
 * Heap management - Free-list allocator
 *
 * The assembler writes four 32-bit words at the very start of
 * memory (byte addresses 0, 4, 8, 12):
 *
 *   [0]  heap_start — byte address of first heap word (set by assembler)
 *   [4]  heap_top   — current high-water mark (byte address)
 *   [8]  reserved
 *   [12] reserved
 *
 * malloc() accepts a byte count and rounds up to the nearest 4-byte word.
 * All block pointers and size fields are in 4-byte WORDS internally.
 *
 * Each heap block has a 3-word (12-byte) header followed by user data:
 *
 *   blk[0]  size — user-data words in this block (header NOT counted)
 *   blk[1]  free — 1 = free, 0 = allocated
 *   blk[2]  next — next free-block pointer (as int byte addr), 0 = end
 *
 * The free list is kept sorted by address so that adjacent free
 * blocks can be coalesced in O(1) on every free().
 * ============================================================= */

#define MALLOC_HDRSIZE   3    /* header words per block (12 bytes)    */
#define MALLOC_MIN_SPLIT 1    /* min user words to create a split     */

static int  _heap_inited = 0;
static int *_free_head   = 0; /* head of free list; 0 = empty         */

/* _heap_init: called once on the first malloc/calloc/free.
 * Reads heap_start from [0] and primes heap_top in [4]. */
static void _heap_init(void) {
    int *mem;
    mem          = (int *)0;
    mem[1]       = mem[0];   /* heap_top = heap_start: heap is empty */
    _free_head   = 0;
    _heap_inited = 1;
}

/* malloc: allocate 'size' bytes from the heap.
 * Internally rounds up to the nearest 4-byte word.
 * Returns a pointer to the usable data area, or 0 on failure.
 * Uses first-fit; splits the block when the surplus is large enough. */
void *malloc(int size) {
    int *prev;
    int *curr;
    int *split;
    int *blk;
    int *mem;
    int  words;
    int  total;

    if (!_heap_inited) _heap_init();
    if (size <= 0) return 0;

    /* Round byte size up to whole words */
    words = (size + 3) / 4;

    /* --- First-fit search through the address-sorted free list --- */
    prev = 0;
    curr = _free_head;
    while (curr != 0) {
        if (curr[0] >= words) {
            /* Found a usable block */
            if (curr[0] >= words + MALLOC_HDRSIZE + MALLOC_MIN_SPLIT) {
                /* Split: carve a new free block from the tail */
                split    = curr + MALLOC_HDRSIZE + words;
                split[0] = curr[0] - words - MALLOC_HDRSIZE;
                split[1] = 1;           /* free              */
                split[2] = curr[2];     /* inherit next ptr  */
                if (prev != 0)
                    prev[2] = (int)split;
                else
                    _free_head = split;
                curr[0] = words;
            } else {
                /* Use the whole block: remove from free list */
                if (prev != 0)
                    prev[2] = curr[2];
                else
                    _free_head = (int *)curr[2];
            }
            curr[1] = 0;    /* mark allocated */
            curr[2] = 0;
            return (void *)(curr + MALLOC_HDRSIZE);
        }
        prev = curr;
        curr = (int *)curr[2];
    }

    /* --- No suitable free block: extend the heap --- */
    mem   = (int *)0;
    total = MALLOC_HDRSIZE + words;
    blk   = (int *)mem[1];              /* current heap_top = new block   */
    mem[1] = (int)(blk + total);        /* advance heap_top by total words */

    blk[0] = words;
    blk[1] = 0;     /* allocated */
    blk[2] = 0;
    return (void *)(blk + MALLOC_HDRSIZE);
}

/* free: return a malloc'd block to the heap.
 * Adjacent free blocks are coalesced to prevent fragmentation.
 * Passing 0 (NULL) is a safe no-op. */
void free(void *ptr) {
    int *blk;
    int *prev;
    int *curr;
    int *adj;

    if (ptr == 0) return;

    blk      = (int *)ptr - MALLOC_HDRSIZE;
    blk[1]   = 1;   /* mark free */

    /* --- Insert into free list, sorted ascending by address --- */
    prev = 0;
    curr = _free_head;
    while (curr != 0 && curr < blk) {
        prev = curr;
        curr = (int *)curr[2];
    }
    blk[2] = (int)curr;
    if (prev != 0)
        prev[2] = (int)blk;
    else
        _free_head = blk;

    /* --- Coalesce blk with its successor (curr) if adjacent --- */
    if (curr != 0) {
        adj = blk + MALLOC_HDRSIZE + blk[0];
        if (adj == curr) {
            blk[0] = blk[0] + MALLOC_HDRSIZE + curr[0];
            blk[2] = curr[2];
        }
    }

    /* --- Coalesce predecessor (prev) with blk if adjacent --- */
    if (prev != 0) {
        adj = prev + MALLOC_HDRSIZE + prev[0];
        if (adj == blk) {
            prev[0] = prev[0] + MALLOC_HDRSIZE + blk[0];
            prev[2] = blk[2];
        }
    }
}

/* calloc: allocate nmemb*size bytes, zero-initialised.
 * Returns 0 on overflow or allocation failure. */
void *calloc(int nmemb, int size) {
    int   total;
    void *ptr;

    if (nmemb <= 0 || size <= 0) return 0;
    total = nmemb * size;
    ptr   = malloc(total);
    if (ptr != 0)
        memset((char *)ptr, 0, total);
    return ptr;
}

/* realloc: resize an existing allocation.
 *   ptr == 0          => behaves like malloc(new_size)
 *   new_size == 0     => behaves like free(ptr), returns 0
 *   new_size <= old   => returns ptr unchanged (no shrink)
 * Returns the (possibly new) data pointer, or 0 on failure.
 * old_size and new_size are in bytes. */
void *realloc(void *ptr, int new_size) {
    int  *blk;
    int   old_words;
    int   new_words;
    void *new_ptr;

    if (ptr == 0)       return malloc(new_size);
    if (new_size <= 0)  { free(ptr); return 0; }

    blk       = (int *)ptr - MALLOC_HDRSIZE;
    old_words = blk[0];
    new_words = (new_size + 3) / 4;
    if (new_words <= old_words) return ptr;   /* already fits */

    new_ptr = malloc(new_size);
    if (new_ptr == 0) return 0;
    memcpy((char *)new_ptr, (char *)ptr, old_words * 4);
    free(ptr);
    return new_ptr;
}

/* heap_words_used: count 4-byte words currently allocated.
 * Walks all blocks linearly from heap_start to heap_top. */
int heap_words_used(void) {
    int *mem;
    int *blk;
    int  used;

    if (!_heap_inited) return 0;
    mem  = (int *)0;
    blk  = (int *)mem[0];   /* heap_start byte address */
    used = 0;
    while (blk < (int *)mem[1]) {
        if (blk[1] == 0)    /* allocated */
            used = used + blk[0];
        blk = blk + MALLOC_HDRSIZE + blk[0];
    }
    return used;
}

/* heap_words_free: count 4-byte words sitting in free blocks. */
int heap_words_free(void) {
    int *curr;
    int  free_words;

    free_words = 0;
    curr       = _free_head;
    while (curr != 0) {
        free_words = free_words + curr[0];
        curr = (int *)curr[2];
    }
    return free_words;
}
