/* Mount the emulator's SD card (an unpartitioned ext4 image) read-only on
 * /data, for tools/cachesim-ic/run.sh: minish has no mount(8). */
#include <stdio.h>
#include <sys/mount.h>
#include <sys/stat.h>

int main(void)
{
	mkdir("/dev", 0755);
	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL); /* may be mounted already */
	mkdir("/data", 0755);
	if (mount("/dev/mmcblk0", "/data", "ext4", MS_RDONLY, NULL)) {
		perror("mntdata: mount /dev/mmcblk0");
		return 1;
	}
	puts("mntdata: /dev/mmcblk0 on /data");
	return 0;
}
