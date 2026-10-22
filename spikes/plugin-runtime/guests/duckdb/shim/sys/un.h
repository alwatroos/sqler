/* SPDX-License-Identifier: Apache-2.0 */
/* SPDX-FileCopyrightText: The SQLer Authors */
/* wasi-libc's sockaddr_un has no sun_path; httplib (inside DuckDB) names the member. Never used at run time. */
#ifndef SPIKE_SYS_UN_H
#define SPIKE_SYS_UN_H
#include <sys/socket.h>
struct sockaddr_un {
    sa_family_t sun_family;
    char sun_path[108];
};
#endif
