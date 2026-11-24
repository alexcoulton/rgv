/* RGV: minimal <regex.h> for MinGW, which has none (upstream HTSlib links libsystre/libtre).
 * HTSlib needs it only for the `=~` / `!~` operators of filter expressions (hts_expr.c),
 * which RGV never uses. regcomp() fails, so such an expression is rejected with an error. */
#ifndef RGV_WINDOWS_COMPAT_REGEX_H
#define RGV_WINDOWS_COMPAT_REGEX_H

#include <stddef.h>

typedef struct { size_t re_nsub; } regex_t;
typedef struct { long rm_so, rm_eo; } regmatch_t;

#define REG_EXTENDED 1
#define REG_NOSUB 4
#define REG_NOMATCH 1
#define REG_BADPAT 2

static inline int regcomp(regex_t *preg, const char *pattern, int cflags) {
    (void) pattern; (void) cflags;
    if (preg) preg->re_nsub = 0;
    return REG_BADPAT;
}

static inline int regexec(const regex_t *preg, const char *string, size_t nmatch,
                          regmatch_t pmatch[], int eflags) {
    (void) preg; (void) string; (void) nmatch; (void) pmatch; (void) eflags;
    return REG_NOMATCH;
}

static inline size_t regerror(int errcode, const regex_t *preg, char *errbuf,
                              size_t errbuf_size) {
    static const char message[] = "regular expressions are unavailable on Windows";
    (void) errcode; (void) preg;
    if (errbuf && errbuf_size) {
        size_t n = sizeof message < errbuf_size ? sizeof message : errbuf_size;
        for (size_t i = 0; i + 1 < n; i++) errbuf[i] = message[i];
        errbuf[n - 1] = '\0';
    }
    return sizeof message;
}

static inline void regfree(regex_t *preg) { (void) preg; }

#endif
