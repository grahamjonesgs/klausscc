/* objhash FILE: two 64-bit hashes and the size of the ELF32 object FILE,
 * "OBJHASH <fnv1a> <mix> <size>", with the bytes of its .comment section
 * (the compiler's version string and git revision) left out.
 * tlbord/run.sh HASH=1 runs it on the compile's object in the guest (the guest
 * has no md5sum, and a hex dump through the UART can contain the prompt "$ ").
 * The host reference is the same source built natively, run on an object the
 * host clang compiled from the same input. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

static uint32_t rd32(const unsigned char *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }
static uint32_t rd16(const unsigned char *p) { return p[0] | p[1] << 8; }

int main(int argc, char **argv)
{
	FILE *f;
	unsigned char *buf;
	size_t n = 0, cap = 1 << 16, i;
	uint64_t a = 1469598103934665603ULL, b = 0x9e3779b97f4a7c15ULL;
	size_t skip0 = 0, skip1 = 0;

	if (argc != 2 || !(f = fopen(argv[1], "rb")) || !(buf = malloc(cap)))
		return 1;
	for (;;) {
		size_t r = fread(buf + n, 1, cap - n, f);
		n += r;
		if (n < cap)
			break;
		cap *= 2;
		if (!(buf = realloc(buf, cap)))
			return 1;
	}
	if (n > 52 && !memcmp(buf, "\177ELF\1\1", 6)) {
		uint32_t shoff = rd32(buf + 0x20);
		unsigned shentsize = rd16(buf + 0x2e), shnum = rd16(buf + 0x30), shstr = rd16(buf + 0x32);
		if (shstr < shnum && shoff + (size_t)shnum * shentsize <= n) {
			const unsigned char *sh = buf + shoff;
			uint32_t stroff = rd32(sh + shstr * shentsize + 16);
			for (i = 0; i < shnum; i++) {
				const unsigned char *s = sh + i * shentsize;
				uint32_t nm = rd32(s), off = rd32(s + 16), sz = rd32(s + 20);
				if (stroff + nm < n && !strcmp((char *)buf + stroff + nm, ".comment") && (size_t)off + sz <= n) {
					skip0 = off;
					skip1 = off + sz;
				}
			}
		}
	}
	for (i = 0; i < n; i++) {
		unsigned c = (i >= skip0 && i < skip1) ? 0 : buf[i];
		a = (a ^ c) * 1099511628211ULL;
		b = (b << 7 | b >> 57) ^ (a + i * 0x100000001b3ULL);
	}
	printf("OBJHASH %016llx %016llx %llu\n", (unsigned long long)a, (unsigned long long)b, (unsigned long long)n);
	return 0;
}
