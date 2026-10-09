/* Minimal native probe: libc only, with no application framework startup. */
#include <stdio.h>
#include <string.h>

extern char **environ;

int main(int argc, char **argv) {
    size_t count = 0;
    int mismatch = 0;

    for (char **entry = environ; *entry != NULL; ++entry) {
        int matched = 0;
        ++count;
        for (int expected = 1; expected < argc; ++expected) {
            if (strcmp(*entry, argv[expected]) == 0) {
                matched = 1;
                break;
            }
        }
        if (!matched) {
            size_t name_length = strcspn(*entry, "=");
            fprintf(stderr, "unexpected child environment variable name: %.*s\n",
                    (int)name_length, *entry);
            mismatch = 1;
        }
    }

    if (count != (size_t)(argc - 1)) {
        fprintf(stderr, "expected %d environment variables, observed %zu\n",
                argc - 1, count);
        mismatch = 1;
    }
    return mismatch;
}
