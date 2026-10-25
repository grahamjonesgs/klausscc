#include "libc.h"

int main() {
    int *mem;
    mem = (int *)0;
    print_str("heap_start=");
    print_hex_prefix(mem[0]);
    newline();
    return 0;
}
