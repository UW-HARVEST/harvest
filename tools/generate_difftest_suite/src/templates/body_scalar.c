    {RET} c_result = c_fn({ARGS});
    {RET} rust_result = rust_fn({ARGS});
    if (c_result == rust_result) { printf("PASS diff_{TEST_ID} {FN_NAME}\n"); (*passed)++; }
    else { printf("FAIL diff_{TEST_ID} {FN_NAME} mismatch\n"); (*failed)++; }
}

