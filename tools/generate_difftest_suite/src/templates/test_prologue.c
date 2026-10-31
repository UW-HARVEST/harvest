static void diff_{TEST_ID}(int *passed, int *failed) {
    {RET} (*c_fn)({PARAM_TYPES}) = dlsym(c_lib, "{FN_NAME}");
    {RET} (*rust_fn)({PARAM_TYPES}) = dlsym(rust_lib, "{FN_NAME}");
    if (!c_fn || !rust_fn) { printf("WARN diff_{TEST_ID} {FN_NAME} symbol not found\n"); return; }
{ARG_DECLS}