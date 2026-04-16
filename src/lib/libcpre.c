#line 1 "lib/libc.c"

#line 8 "lib/libc.c"


extern void _uart_tx_hex(int val);
extern void _uart_newline(void);
extern void _uart_tx_char(int ch);
extern int  _uart_rx_char(void);
extern int  _uart_rx_char_nb(void);


void print_unsigned(unsigned n);
void _print_neg(int neg, int orig);
void print_str(char *s);


#line 24 "lib/libc.c"

void putchar(int ch) {
    _uart_tx_char(ch);
}


int getchar(void) {
    return _uart_rx_char();
}


#line 37 "lib/libc.c"
int getchar_nb(void) {
    return _uart_rx_char_nb();
}

void puts(char *s) {
    while (*s != 0) {
        putchar(*s);
        s = s + 1;
    }
    putchar(10);
}


void print_str(char *s) {
    while (*s != 0) {
        putchar(*s);
        s = s + 1;
    }
}


#line 60 "lib/libc.c"


void print_unsigned(unsigned n) {

    if (n >= 10)
        print_unsigned(n / 10);
    putchar((n % 10) + 48);
}


void print_int(int n) {
    if (n < 0) {
        putchar(45);

        _print_neg(-n, n);
        return;
    }
    print_unsigned(n);
}


#line 82 "lib/libc.c"
void _print_neg(int neg, int orig) {
    if (neg < 0) {

        putchar(50); putchar(49); putchar(52); putchar(55);
        putchar(52); putchar(56); putchar(51); putchar(54);
        putchar(52); putchar(56);
    } else {
        print_unsigned(neg);
    }
}


void print_hex(int val) {
    _uart_tx_hex(val);
}


void print_hex_prefix(int val) {
    putchar(48);
    putchar(120);
    _uart_tx_hex(val);
}


void newline(void) {
    _uart_newline();
}


#line 115 "lib/libc.c"


#line 119 "lib/libc.c"


int strlen(char *s) {
    int len;
    len = 0;
    while (*s != 0) {
        len = len + 1;
        s = s + 1;
    }
    return len;
}


int strcmp(char *a, char *b) {
    while (*a != 0 && *a == *b) {
        a = a + 1;
        b = b + 1;
    }
    return *a - *b;
}


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


#line 156 "lib/libc.c"


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


#line 187 "lib/libc.c"


int abs(int n) {
    if (n < 0) return -n;
    return n;
}


int min(int a, int b) {
    if (a < b) return a;
    return b;
}

int max(int a, int b) {
    if (a > b) return a;
    return b;
}


void swap(int *a, int *b) {
    int t;
    t = *a;
    *a = *b;
    *b = t;
}


#line 236 "lib/libc.c"




static int  _heap_inited = 0;
static int *_heap_top    = 0;
static int *_free_head   = 0;


#line 246 "lib/libc.c"
static void _heap_init(void) {
    int *mem;
    mem          = (int *)0;
    _heap_top    = (int *)mem[0];
    _free_head   = 0;
    _heap_inited = 1;
}


#line 258 "lib/libc.c"
void *malloc(int size) {
    int *prev;
    int *curr;
    int *split;
    int *blk;
    int  words;
    int  total;

    if (!_heap_inited) _heap_init();
    if (size <= 0) return 0;


    words = (size + 3) / 4;


    prev = 0;
    curr = _free_head;
    while (curr != 0) {
        if (curr[0] >= words) {

            if (curr[0] >= words + 3 + 1) {

                split    = curr + 3 + words;
                split[0] = curr[0] - words - 3;
                split[1] = 1;
                split[2] = curr[2];
                if (prev != 0)
                    prev[2] = (int)split;
                else
                    _free_head = split;
                curr[0] = words;
            } else {

                if (prev != 0)
                    prev[2] = curr[2];
                else
                    _free_head = (int *)curr[2];
            }
            curr[1] = 0;
            curr[2] = 0;
            return (void *)(curr + 3);
        }
        prev = curr;
        curr = (int *)curr[2];
    }


    total      = 3 + words;
    blk        = _heap_top;
    _heap_top  = blk + total;

    blk[0] = words;
    blk[1] = 0;
    blk[2] = 0;
    return (void *)(blk + 3);
}


#line 318 "lib/libc.c"
void free(void *ptr) {
    int *blk;
    int *prev;
    int *curr;
    int *adj;

    if (ptr == 0) return;

    blk      = (int *)ptr - 3;
    blk[1]   = 1;


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


    if (curr != 0) {
        adj = blk + 3 + blk[0];
        if (adj == curr) {
            blk[0] = blk[0] + 3 + curr[0];
            blk[2] = curr[2];
        }
    }


    if (prev != 0) {
        adj = prev + 3 + prev[0];
        if (adj == blk) {
            prev[0] = prev[0] + 3 + blk[0];
            prev[2] = blk[2];
        }
    }
}


#line 363 "lib/libc.c"
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


#line 381 "lib/libc.c"
void *realloc(void *ptr, int new_size) {
    int  *blk;
    int   old_words;
    int   new_words;
    void *new_ptr;

    if (ptr == 0)       return malloc(new_size);
    if (new_size <= 0)  { free(ptr); return 0; }

    blk       = (int *)ptr - 3;
    old_words = blk[0];
    new_words = (new_size + 3) / 4;
    if (new_words <= old_words) return ptr;

    new_ptr = malloc(new_size);
    if (new_ptr == 0) return 0;
    memcpy((char *)new_ptr, (char *)ptr, old_words * 4);
    free(ptr);
    return new_ptr;
}


#line 405 "lib/libc.c"
int heap_words_used(void) {
    int *mem;
    int *blk;
    int  used;

    if (!_heap_inited) return 0;
    mem  = (int *)0;
    blk  = (int *)mem[0];
    used = 0;
    while (blk != _heap_top) {
        if (blk[1] == 0)
            used = used + blk[0];
        blk = blk + 3 + blk[0];
    }
    return used;
}


#line 424 "lib/libc.c"
int heap_get_top(void) {
    return (int)_heap_top;
}


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
