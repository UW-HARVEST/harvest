#include <stdio.h>
#include <string.h>
#include <dlfcn.h>
#include <limits.h>
#include <stdint.h>
#include <stdbool.h>

static void *c_lib;
static void *rust_lib;

