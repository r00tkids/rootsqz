all: helloworld

CFLAGS += -arch arm64 -Oz
# Not really needed for C, but good practice to disable in small binaries
CFLAGS += -fno-exceptions -fno-rtti
CFLAGS += -ffunction-sections -fdata-sections
CFLAGS += -fno-unwind-tables -fno-asynchronous-unwind-tables
CFLAGS += -Wl,-dead_strip
CFLAGS += -Wl,-x

helloworld: helloworld.c
	clang $(CFLAGS) -o $@ $<