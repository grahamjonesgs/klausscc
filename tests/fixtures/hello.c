/*
 * hello.c — Step 22 smoke test for KlaussCPU
 *
 * Prints over the UART with uart_puts and uart_newline and waits with
 * delay_hw; all three come from a separate uart_stubs.c (not in this repo).
 */

void uart_puts(const char *s);
void uart_newline(void);
void delay_hw(unsigned long long cycles);

int main(int argc, char **argv) {
    uart_puts("Hello, KlaussCPU! 1");
    uart_newline();
    delay_hw(5000);

    uart_puts("Hello, KlaussCPU! 2");
    uart_newline();
    delay_hw(5000);

    return 0;
} 
