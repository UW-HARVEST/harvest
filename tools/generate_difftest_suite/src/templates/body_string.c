    const char *c_result = (const char *)c_fn({ARGS});
    const char *rust_result = (const char *)rust_fn({ARGS});
    if (c_result == NULL && rust_result == NULL) { printf("PASS diff_{TEST_ID} {FN_NAME}\n"); (*passed)++; }
    else if (c_result != NULL && rust_result != NULL && strcmp(c_result, rust_result) == 0) { printf("PASS diff_{TEST_ID} {FN_NAME}\n"); (*passed)++; }
    else { printf("FAIL diff_{TEST_ID} {FN_NAME} mismatch\n"); (*failed)++; }
}

