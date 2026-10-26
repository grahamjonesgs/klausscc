/*
 * hello.c — Step 22 smoke test for KlaussCPU
 *
 * Prints over the UART with uart_puts and uart_newline, which come from
 * a separate uart_stubs.c (not in this repo).
 */

void uart_puts(const char *s);
void uart_newline(void);

extern void putchar(int ch);
extern void print_str(char *s);
extern void print_int(long long n);
extern void newline(void);
extern int  getchar(void);

int main(int argc, char **argv) {
    uart_puts("Hello, KlaussCPU! 1");
    uart_puts("xxHello, KlaussCPU! 44");
    //print_str("Hello, KlaussCPU! 4"); newline();
    //uart_puts("Hello, KlaussCPU! 3");

    uart_newline();

    return 0;
} 
