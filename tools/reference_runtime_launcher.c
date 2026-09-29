#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/*
 * Relocatable launcher for TonePoet's package-private Reference runtime.
 *
 * Public entry points live at <root>/bin/<tool>.  Each is a byte-identical
 * copy of this statically linked launcher.  The staged payload and the exact
 * glibc loader copied from its Nix closure are named by relative symlinks at:
 *
 *   <root>/.tonepoet-runtime/payload/<tool>
 *   <root>/.tonepoet-runtime/loaders/<tool>
 *
 * The launcher is static so locating the private loader cannot itself depend
 * on the host dynamic linker.  It invokes that loader explicitly, making the
 * payload's original absolute PT_INTERP inert.  The staging tool rewrites ELF
 * search paths to $ORIGIN-relative private paths and marks dynamic objects
 * NODEFLIB, while --inhibit-cache prevents host ld.so.cache from participating.
 */

static const char *const allowed_tools[] = {
    "sox",
    "ffmpeg",
    "ffprobe",
    "metaflac",
    "wvtag",
    "AtomicParsley",
};

static void die(const char *message) {
    int saved = errno;
    if (saved != 0) {
        fprintf(stderr, "tonepoet-reference-launcher: %s: %s\n", message, strerror(saved));
    } else {
        fprintf(stderr, "tonepoet-reference-launcher: %s\n", message);
    }
    _exit(126);
}

static int allowed_tool(const char *name) {
    size_t count = sizeof(allowed_tools) / sizeof(allowed_tools[0]);
    for (size_t i = 0; i < count; ++i) {
        if (strcmp(name, allowed_tools[i]) == 0) {
            return 1;
        }
    }
    return 0;
}

static char *self_path(void) {
    size_t size = 256;
    for (;;) {
        char *buffer = malloc(size);
        if (buffer == NULL) {
            die("out of memory resolving /proc/self/exe");
        }
        ssize_t n = readlink("/proc/self/exe", buffer, size - 1);
        if (n < 0) {
            free(buffer);
            die("cannot read /proc/self/exe");
        }
        if ((size_t)n < size - 1) {
            buffer[n] = '\0';
            if (strstr(buffer, " (deleted)") != NULL) {
                free(buffer);
                errno = 0;
                die("launcher executable was replaced while running");
            }
            return buffer;
        }
        free(buffer);
        if (size > (size_t)PATH_MAX * 16U) {
            errno = ENAMETOOLONG;
            die("launcher path is unreasonably long");
        }
        size *= 2;
    }
}

static char *join3(const char *a, const char *b, const char *c) {
    size_t alen = strlen(a);
    size_t blen = strlen(b);
    size_t clen = strlen(c);
    if (alen > SIZE_MAX - blen - clen - 3U) {
        errno = ENAMETOOLONG;
        die("runtime path length overflow");
    }
    size_t total = alen + blen + clen + 3U;
    char *out = malloc(total);
    if (out == NULL) {
        die("out of memory constructing runtime path");
    }
    int n = snprintf(out, total, "%s/%s/%s", a, b, c);
    if (n < 0 || (size_t)n >= total) {
        free(out);
        errno = ENAMETOOLONG;
        die("runtime path construction failed");
    }
    return out;
}

static void clear_loader_injection_environment(void) {
    /* Reference subprocesses already use ClearAndSet.  Strip loader injection
     * here as a second boundary so direct package entry-point use cannot make
     * host libraries supersede the attested private closure. */
    static const char *const names[] = {
        "LD_LIBRARY_PATH",
        "LD_PRELOAD",
        "LD_AUDIT",
        "LD_DEBUG",
        "LD_PROFILE",
        "LD_TRACE_LOADED_OBJECTS",
        "LD_ORIGIN_PATH",
        "LD_HWCAP_MASK",
        "GLIBC_TUNABLES",
    };
    size_t count = sizeof(names) / sizeof(names[0]);
    for (size_t i = 0; i < count; ++i) {
        if (unsetenv(names[i]) != 0) {
            die("cannot sanitize dynamic-loader environment");
        }
    }
}

int main(int argc, char **argv) {
    if (argc < 1 || argv == NULL || argv[0] == NULL) {
        errno = EINVAL;
        die("invalid process argument vector");
    }

    char *self = self_path();
    char *slash = strrchr(self, '/');
    if (slash == NULL || slash == self || slash[1] == '\0') {
        free(self);
        errno = EINVAL;
        die("launcher is not installed under a bin directory");
    }
    const char *tool = slash + 1;
    if (!allowed_tool(tool)) {
        free(self);
        errno = EINVAL;
        die("launcher basename is not a qualified Reference tool");
    }

    *slash = '\0';
    char *bin_slash = strrchr(self, '/');
    if (bin_slash == NULL || strcmp(bin_slash + 1, "bin") != 0) {
        free(self);
        errno = EINVAL;
        die("launcher must live at <runtime-root>/bin/<tool>");
    }
    *bin_slash = '\0';
    const char *root = self;

    char *loader_link = join3(root, ".tonepoet-runtime/loaders", tool);
    char *payload_link = join3(root, ".tonepoet-runtime/payload", tool);

    /*
     * glibc derives $ORIGIN for the main object from the pathname supplied to
     * the loader.  The manifest-visible mappings above are symlinks, so passing
     * the mapping path would incorrectly make `.tonepoet-runtime/payload` the
     * origin instead of the copied ELF's `nix/store/.../bin` directory.  Resolve
     * both mappings first and execute the actual staged objects.
     */
    char *loader = realpath(loader_link, NULL);
    if (loader == NULL) {
        die("cannot resolve package-private dynamic loader");
    }
    char *payload = realpath(payload_link, NULL);
    if (payload == NULL) {
        die("cannot resolve package-private payload");
    }
    free(loader_link);
    free(payload_link);

    const char *list_mode = getenv("TONEPOET_REFERENCE_LAUNCHER_LIST");
    int list_only = list_mode != NULL && strcmp(list_mode, "1") == 0;
    if (unsetenv("TONEPOET_REFERENCE_LAUNCHER_LIST") != 0) {
        die("cannot clear launcher validation flag");
    }
    clear_loader_injection_environment();

    /* loader argv: loader, --inhibit-cache, [--list | --argv0 tool payload], args... */
    size_t extra = list_only ? 4U : 6U;
    if ((size_t)argc > SIZE_MAX / sizeof(char *) - extra) {
        errno = E2BIG;
        die("argument vector is too large");
    }
    char **next = calloc((size_t)argc + extra, sizeof(char *));
    if (next == NULL) {
        die("out of memory constructing loader arguments");
    }

    size_t i = 0;
    next[i++] = loader;
    next[i++] = "--inhibit-cache";
    if (list_only) {
        next[i++] = "--list";
        next[i++] = payload;
    } else {
        next[i++] = "--argv0";
        next[i++] = (char *)tool;
        next[i++] = payload;
        for (int arg = 1; arg < argc; ++arg) {
            next[i++] = argv[arg];
        }
    }
    next[i] = NULL;

    execv(loader, next);
    die("cannot execute package-private dynamic loader");
}
