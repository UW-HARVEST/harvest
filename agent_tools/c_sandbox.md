### Tool: `c_sandbox` — Run a C snippet and capture its exact stdout

**Location:** `{AGENT_TOOLS_DIR}/c_sandbox.py`

Compiles a complete C program with `gcc -O0` and returns its exact stdout bytes.
Use this whenever you need to know what C actually produces — printf edge cases,
integer promotion results, scanf behavior, floating-point formatting — rather than
reasoning about it.

**Usage:**

```
# Pass code via stdin (heredoc)
python3 {AGENT_TOOLS_DIR}/c_sandbox.py << 'EOF'
<your C program here>
EOF

# Or pass a file
python3 {AGENT_TOOLS_DIR}/c_sandbox.py mytest.c
```

**Output:** The program's exact stdout. Compile errors and runtime errors go to
stderr with a descriptive prefix (`COMPILE ERROR:`, `RUNTIME EXIT:`).

**Example — verify printf float formatting:**

```
python3 {AGENT_TOOLS_DIR}/c_sandbox.py << 'EOF'
#include <stdio.h>
#include <math.h>
int main() {
    printf("%.9g\n", 0.1);
    printf("%.9g\n", NAN);
    printf("%.9g\n", INFINITY);
    printf("%.9g\n", -0.0);
    printf("%.9g\n", 1.2345678901234567e-10);
}
EOF
```

Output:
```
0.1
nan
inf
-0
1.23456789e-10
```

**Example — verify integer arithmetic semantics:**

```
python3 {AGENT_TOOLS_DIR}/c_sandbox.py << 'EOF'
#include <stdio.h>
int main() {
    unsigned char x = 0;
    printf("%d\n", x - 1);          /* C promotes to int: prints -1 */
    printf("%d\n", (x - 1) & 0xFF); /* prints 255 */
    int wrap = 0;
    printf("%d\n", (wrap - 1) & 255); /* signed int: prints 255 */
}
EOF
```

**Example — verify scanf consumption:**

```
python3 {AGENT_TOOLS_DIR}/c_sandbox.py << 'EOF'
#include <stdio.h>
int main() {
    int a, b;
    int n = sscanf("  42  -7  ", "%d %d", &a, &b);
    printf("n=%d a=%d b=%d\n", n, a, b);
}
EOF
```

**Notes:**
- `-lm` is always linked; no other libraries are available.
- Timeout: 10 seconds.
- Do NOT use this tool to run untrusted code; it compiles and executes as the current user.
