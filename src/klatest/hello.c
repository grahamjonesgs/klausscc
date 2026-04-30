/*
 * hello.c — Step 22 smoke test for KlaussCPU
 *
 * Uses hardware UART instructions via uart_stubs.c.
 * No memory-mapped addresses needed — the CPU handles UART internally.
 */

void uart_puts(const char *s);
void uart_newline(void);
void leds(unsigned long long val);
void seg7(unsigned long long val);
void delay_hw(unsigned long long cycles);

int main(int argc, char **argv) {
    leds(0x0001);
    seg7(0x0001);

    uart_puts("Hello, KlaussCPU! 1");
    uart_newline();
    delay_hw(5000);

    uart_puts("Hello, KlaussCPU! 2");
    uart_newline();
    delay_hw(5000);

    leds(0x0003);
    seg7(0x0003);

    return 0;
} 
