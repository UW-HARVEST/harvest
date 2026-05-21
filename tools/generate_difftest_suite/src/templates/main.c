int main(int argc, char *argv[]) {
    if (argc != 3) { fprintf(stderr, "Usage: %s <libc.so> <librust.so>\n", argv[0]); return 1; }
    c_lib = dlopen(argv[1], RTLD_LOCAL | RTLD_NOW);
    rust_lib = dlopen(argv[2], RTLD_LOCAL | RTLD_NOW);
    if (!c_lib || !rust_lib) { fprintf(stderr, "dlopen failed: %s\n", dlerror()); return 1; }
    int passed = 0, failed = 0;
{TEST_CALLS}    printf("SUMMARY: %d passed, %d failed out of %d tests\n", passed, failed, passed + failed);
    return failed > 0 ? 1 : 0;
}
