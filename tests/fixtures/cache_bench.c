/* cache_bench.c - Visual cache performance benchmark
 *
 * Walks a 100-word array thousands of times per print.
 * Watch serial output: counter advances faster with cache ON.
 *
 * Cache OFF: every access hits DDR2 -> slow counter
 * Cache ON:  array stays cached     -> fast counter
 */

extern void _uart_tx_hex(int val);
extern void _uart_newline(void);

int buf[100];

int main() {
    int counter;
    int batch;
    int rep;
    int i;
    int sum;

    /* Fill array */
    i = 0;
    while (i < 100) {
        buf[i] = i;
        i = i + 1;
    }

    /* Benchmark: 50 batches x 100 passes = 5000 array walks per print.
     * That is ~1.5 million memory ops between each printed number.
     */
    counter = 0;
    while (counter < 127) {
        batch = 0;
        while (batch < 50) {
            rep = 0;
            while (rep < 100) {
                sum = 0;
                i = 0;
                while (i < 100) {
                    sum = sum + buf[i];
                    buf[i] = sum;
                    i = i + 1;
                }
                rep = rep + 1;
            }
            batch = batch + 1;
        }

        _uart_tx_hex(counter);
        _uart_newline();
        counter = counter + 1;
    }

    return sum;
}
