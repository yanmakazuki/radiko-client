#include <stdarg.h>
#include <alsa/error.h>

/* ALSA の既定ハンドラは stderr へ "ALSA lib ..." を書く。
 * printf 引数は読まない。診断を戻すときはハンドラを差し替えない。
 */
static void radiko_alsa_silent(
    const char *file,
    int line,
    const char *function,
    int err,
    const char *fmt,
    ...
) {
    (void)file;
    (void)line;
    (void)function;
    (void)err;
    (void)fmt;
}

void radiko_install_silent_alsa_error_handler(void) {
    snd_lib_error_set_handler(radiko_alsa_silent);
}
