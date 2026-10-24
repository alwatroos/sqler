#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: The SQLer Authors
#
# Test-only stand-in for `gh` (used by tag-release-validate-test.sh, copied to <tmp>/bin/gh). Canned output comes from
# FAKE_GH_TAG_REFS, FAKE_GH_RELEASE_RUN and FAKE_GH_CI_RUN; every call is appended to FAKE_GH_LOG. FAKE_GH_FAIL set to
# api, release or ci makes that call exit 1. Anything else exits 2.
set -eu

printf '%s\n' "$*" >>"${FAKE_GH_LOG:-/dev/null}"

case "$*" in
    api*matching-refs*)
        [ "${FAKE_GH_FAIL:-}" = api ] && exit 1
        printf '%s\n' "${FAKE_GH_TAG_REFS:-}"
        ;;
    run\ list*release.yml*)
        [ "${FAKE_GH_FAIL:-}" = release ] && exit 1
        printf '%s\n' "${FAKE_GH_RELEASE_RUN:-}"
        ;;
    run\ list*ci.yml*)
        [ "${FAKE_GH_FAIL:-}" = ci ] && exit 1
        printf '%s\n' "${FAKE_GH_CI_RUN:-}"
        ;;
    *) exit 2 ;;
esac
