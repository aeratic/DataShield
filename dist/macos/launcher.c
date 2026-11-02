// DataShield.app main executable: starts the bundled kitty with DataShield's
// terminal profile and binary. A compiled launcher (not a script) because
// macOS refuses to spawn a signed bundle whose main executable is a script.
#include <libgen.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char exe[PATH_MAX];
    uint32_t n = sizeof exe;
    if (_NSGetExecutablePath(exe, &n) != 0) return 1;
    char real[PATH_MAX];
    if (!realpath(exe, real)) return 1;
    char *macos = dirname(real);             // .../Contents/MacOS
    char res[PATH_MAX], kitty[PATH_MAX], conf[PATH_MAX], tui[PATH_MAX], shell[PATH_MAX + 8];
    snprintf(res, sizeof res, "%s/../Resources", macos);
    snprintf(kitty, sizeof kitty, "%s/kitty", macos);
    snprintf(conf, sizeof conf, "%s/datashield.kitty.conf", res);
    snprintf(tui, sizeof tui, "%s/datashield-tui", res);
    snprintf(shell, sizeof shell, "shell=%s", tui);

    setenv("DATASHIELD_OWN_TERM", "1", 1);

    // kitty [opts] datashield-tui [user args...]
    int extra = argc - 1;
    char **args = calloc(12 + extra, sizeof *args);
    int i = 0;
    args[i++] = kitty;
    args[i++] = "--single-instance";
    args[i++] = "--instance-group"; args[i++] = "DataShield";
    args[i++] = "--config";         args[i++] = conf;
    args[i++] = "--override";       args[i++] = shell;
    args[i++] = "--title";          args[i++] = "DataShield";
    args[i++] = tui;
    for (int k = 1; k < argc; k++) args[i++] = argv[k];
    args[i] = NULL;
    execv(kitty, args);
    perror("DataShield: exec kitty");
    return 1;
}
