/* adventure.c - Mini text adventure for FPGA_CPU_32_DDR_cache
 *
 * Compile: build/rcc -target=klacpu adventure.c > adventure.kla
 *
 * Then in crt0.kla:
 *   INCLUDE adventure.kla
 *   INCLUDE lib/libc.kla
 *   INCLUDE lib/uart_stubs.kla
 *
 * Commands: n/s/e/w = move, l = look, g = get, u = use, i = inventory, q = quit
 * Goal: find torch, find key (needs torch), unlock door, escape!
 */

extern void putchar(int ch);
extern void print_str(char *s);
extern void print_int(int n);
extern void newline(void);
extern int  getchar(void);

/* Game state */
int room;
int items;
int turns;
int alive;

#define ROOM_CELL    0
#define ROOM_HALL    1
#define ROOM_ARMORY  2
#define ROOM_DOOR    3
#define ROOM_FREE    4

#define HAS_TORCH    1
#define HAS_KEY      2

/* Read one command character from UART */
int read_cmd(void)
{
    int c;
    int cmd;

    print_str("> ");
    cmd = getchar();
    putchar(cmd);

    /* Drain rest of line until CR or LF */
    if (cmd != 13 && cmd != 10) {
        c = cmd;
        while (c != 13 && c != 10) {
            c = getchar();
        }
    }
    newline();

    /* Uppercase to lowercase */
    if (cmd >= 'A' && cmd <= 'Z') {
        cmd = cmd + 32;
    }
    return cmd;
}

/* Room descriptions */
void look(void)
{
    if (room == ROOM_CELL) {
        print_str("You are in a cold stone cell."); newline();
        print_str("Moonlight seeps through a crack."); newline();
        if (!(items & HAS_TORCH)) {
            print_str("A rusty TORCH sits on the wall."); newline();
        }
        print_str("A passage leads NORTH."); newline();
    }
    else if (room == ROOM_HALL) {
        print_str("You stand in a long hallway."); newline();
        if (items & HAS_TORCH) {
            print_str("Your torch reveals wall scratches:"); newline();
            print_str("THE ARMORY HIDES SECRETS"); newline();
        }
        else {
            print_str("It is very dark here."); newline();
        }
        print_str("Exits: S=cell E=armory N=door"); newline();
    }
    else if (room == ROOM_ARMORY) {
        print_str("You enter a dusty armory."); newline();
        print_str("Broken swords line the walls."); newline();
        if (!(items & HAS_KEY)) {
            if (items & HAS_TORCH) {
                print_str("Torchlight glints off a brass KEY!"); newline();
            }
            else {
                print_str("Something metallic glints..."); newline();
            }
        }
        else {
            print_str("Nothing else of interest here."); newline();
        }
        print_str("Exit: W=hallway"); newline();
    }
    else if (room == ROOM_DOOR) {
        print_str("A massive iron door blocks the way."); newline();
        print_str("There is a keyhole."); newline();
        if (items & HAS_KEY) {
            print_str("The brass key might fit..."); newline();
            print_str("Type u to USE the key."); newline();
        }
        else {
            print_str("It is locked. You need a key."); newline();
        }
        print_str("Exit: S=hallway"); newline();
    }
}

/* Inventory */
void show_inventory(void)
{
    print_str("You carry: ");
    if (items == 0) {
        print_str("nothing");
    }
    if (items & HAS_TORCH) {
        print_str("[torch] ");
    }
    if (items & HAS_KEY) {
        print_str("[key] ");
    }
    newline();
}

/* Pick up items */
void do_get(void)
{
    if (room == ROOM_CELL && !(items & HAS_TORCH)) {
        items = items | HAS_TORCH;
        print_str("You take the torch. Flames dance!"); newline();
    }
    else if (room == ROOM_ARMORY && !(items & HAS_KEY)) {
        if (items & HAS_TORCH) {
            items = items | HAS_KEY;
            print_str("You pick up the brass key."); newline();
        }
        else {
            print_str("You fumble in the dark..."); newline();
        }
    }
    else {
        print_str("Nothing to pick up here."); newline();
    }
}

/* Use items */
void do_use(void)
{
    if (room == ROOM_DOOR && (items & HAS_KEY)) {
        print_str("You insert the key... CLICK!"); newline();
        print_str("The door groans open. Starlight!"); newline();
        room = ROOM_FREE;
    }
    else {
        print_str("Nothing useful to do here."); newline();
    }
}

/* Movement */
void do_move(int dir)
{
    if (room == ROOM_CELL) {
        if (dir == 'n') { room = ROOM_HALL; }
        else { print_str("You cannot go that way."); newline(); return; }
    }
    else if (room == ROOM_HALL) {
        if (dir == 's') { room = ROOM_CELL; }
        else if (dir == 'e') { room = ROOM_ARMORY; }
        else if (dir == 'n') { room = ROOM_DOOR; }
        else { print_str("You cannot go that way."); newline(); return; }
    }
    else if (room == ROOM_ARMORY) {
        if (dir == 'w') { room = ROOM_HALL; }
        else { print_str("You cannot go that way."); newline(); return; }
    }
    else if (room == ROOM_DOOR) {
        if (dir == 's') { room = ROOM_HALL; }
        else { print_str("You cannot go that way."); newline(); return; }
    }
    look();
}

/* Main */
int main(void)
{
    int cmd;

    room = ROOM_CELL;
    items = 0;
    turns = 0;
    alive = 1;

    print_str("========================================"); newline();
    print_str("   DUNGEON ESCAPE"); newline();
    print_str("   Running on FPGA_CPU_32_DDR_cache!"); newline();
    print_str("========================================"); newline();
    print_str("Commands: n/s/e/w=move l=look"); newline();
    print_str("  g=get u=use i=inventory q=quit"); newline();
    newline();
    print_str("You awaken on cold stone..."); newline();
    newline();
    look();

    while (alive) {
        cmd = read_cmd();
        turns = turns + 1;

        if (cmd == 'q') {
            print_str("You surrender to the darkness..."); newline();
            alive = 0;
        }
        else if (cmd == 'l') {
            look();
        }
        else if (cmd == 'i') {
            show_inventory();
        }
        else if (cmd == 'g') {
            do_get();
        }
        else if (cmd == 'u') {
            do_use();
        }
        else if (cmd == 'n' || cmd == 's' || cmd == 'e' || cmd == 'w') {
            do_move(cmd);
        }
        else {
            print_str("Huh? Try n/s/e/w/l/g/u/i/q"); newline();
        }

        if (room == ROOM_FREE) {
            newline();
            print_str("*** YOU ESCAPED THE DUNGEON! ***"); newline();
            print_str("Turns: ");
            print_int(turns);
            newline();
            alive = 0;
        }
    }

    print_str("Game over."); newline();
    return 0;
}
