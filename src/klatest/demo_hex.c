/* demo_hex.c - Demonstrates compiled C with hex output only
 *
 * Does NOT require .word assembler support — all output via TXR.
 * Each result is printed as 8 hex digits followed by newline.
 *
 * Expected UART output:
 *   0000002A  (6 * 7 = 42)
 *   00000078  (factorial(5) = 120)
 *   0037B9AC  (factorial(12) = 3628972 — demonstrates large multiply)
 *   00000015  (gcd(252, 105) = 21)
 *   00000037  (fibonacci(9) = 55)
 *   00000008  (primes 2..20: count = 8)
 *   00000013  (largest prime <= 20 = 19)
 *   000000A8  (sum = 42 + 120 + 6 = 168 — final check)
 */

extern void _uart_tx_hex(int val);
extern void _uart_newline(void);

void print_result(int val) {
    _uart_tx_hex(val);
    _uart_newline();
}

int factorial(int n) {
    int r, i;
    r = 1;
    for (i = 2; i <= n; i = i + 1)
        r = r * i;
    return r;
}

int gcd(int a, int b) {
    while (b != 0) {
        int t;
        t = b;
        b = a % b;
        a = t;
    }
    return a;
}

int fibonacci(int n) {
    int a, b, t, i;
    a = 0;
    b = 1;
    for (i = 0; i < n; i = i + 1) {
        t = a + b;
        a = b;
        b = t;
    }
    return a;
}

int is_prime(int n) {
    int i;
    if (n <= 1) return 0;
    if (n <= 3) return 1;
    i = 2;
    while (i * i <= n) {
        if ((n % i) == 0) return 0;
        i = i + 1;
    }
    return 1;
}

int main() {
    int i, count, last;
    print_str("Hello World");

    /* Basic arithmetic */
    print_result(6 * 7);           /* 0x2A = 42 */

    /* Factorial */
    print_result(factorial(5));     /* 0x78 = 120 */
    print_result(factorial(12));    /* 0x1C8CFC00 = 479001600 */

    /* GCD */
    print_result(gcd(252, 105));   /* 0x15 = 21 */

    /* Fibonacci */
    print_result(fibonacci(9));    /* 0x37 = 55 */

    /* Count primes 2..20 */
    count = 0;
    last = 0;
    for (i = 2; i <= 20; i = i + 1) {
        if (is_prime(i)) {
            count = count + 1;
            last = i;
        }
    }
    print_result(count);           /* 0x08 = 8 */
    print_result(last);            /* 0x13 = 19 */

    /* Sum check */
    print_result(42 + 120 + 6);   /* 0xA8 = 168 */

    return 0;
}
