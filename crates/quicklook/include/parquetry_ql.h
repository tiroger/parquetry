/*
 * parquetry_ql.h — C ABI of the parquetry-quicklook static library.
 *
 * Link against libparquetry_quicklook.a (built with
 * `cargo build --release -p parquetry-quicklook`).
 */
#ifndef PARQUETRY_QL_H
#define PARQUETRY_QL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Render an HTML preview of the Parquet file at `path` (NUL-terminated UTF-8 file-system
 * path), showing at most `max_rows` rows. Only the file footer and the pages needed for the
 * first rows are read.
 *
 * Always returns a UTF-8 HTML document: the preview, or a styled error page if the file can't
 * be read. The buffer is NOT NUL-terminated; its length in bytes is stored in `*out_len`.
 * Returns NULL only if memory allocation fails.
 *
 * The returned buffer must be released with parquetry_ql_free(ptr, *out_len).
 * Thread-safe; never unwinds across the FFI boundary.
 */
uint8_t *_Nullable parquetry_ql_preview(const char *_Nullable path,
                                        uint32_t max_rows,
                                        size_t *_Nullable out_len);

/*
 * Free a buffer returned by parquetry_ql_preview. `len` must be the length reported through
 * `out_len`. Passing NULL is a no-op.
 */
void parquetry_ql_free(uint8_t *_Nullable ptr, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* PARQUETRY_QL_H */
