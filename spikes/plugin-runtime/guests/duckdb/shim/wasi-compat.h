/* SPDX-License-Identifier: Apache-2.0 */
/* SPDX-FileCopyrightText: The SQLer Authors */
/*
 * Force-included (-include) into every DuckDB translation unit by build.sh. It does not touch DuckDB
 * sources; it works around two wasi-libc gaps:
 *  - <langinfo.h> defines the macro ERA, which clashes with DuckDB's DatePartSpecifier::ERA;
 *  - sched_getcpu() does not exist (DuckDB uses it under _GNU_SOURCE).
 */
#include <langinfo.h>
#undef ERA
#include <sched.h>
static inline int sched_getcpu(void) { return 0; }
