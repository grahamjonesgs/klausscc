/* echo_upper.c - Read a line from UART, uppercase it, echo it back.
 *
 * Compile: build/rcc -target=klacpu tests/echo_upper.c > tests/echo_upper.kla
 *
 * Expected interaction:
 *   Enter message: hello world
 *   Uppercase: HELLO WORLD
 */

extern void putchar(int ch);
extern void print_str(int *s);
extern void newline(void);
extern int  getchar(void);

/* "Enter message: " */
int s_prompt[] = { 69,110,116,101,114,32,109,101,115,115,97,103,101,58,32, 0 };

/* "Uppercase: " */
int s_upper[]  = { 85,112,112,101,114,99,97,115,101,58,32, 0 };

int main() {
    int buf[64];
    int len;
    int ch;
    int i;

    print_str(s_prompt);

    /* Read until CR or LF, echoing each character as it arrives */
    len = 0;
    ch = getchar();
    while (ch != 13 && ch != 10 && len < 63) {
        putchar(ch);
        buf[len] = ch;
        len = len + 1;
        ch = getchar();
    }
    buf[len] = 0;
    newline();

    /* Uppercase in-place: a-z (97-122) -> A-Z (65-90) */
    i = 0;
    while (i < len) {
        if (buf[i] >= 97 && buf[i] <= 122)
            buf[i] = buf[i] - 32;
        i = i + 1;
    }

    print_str(s_upper);
    print_str(buf);
    newline();

    return 0;
}
