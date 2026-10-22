;; SPDX-License-Identifier: Apache-2.0
;; SPDX-FileCopyrightText: The SQLer Authors
;; Two linear memories of 600 pages (37.5 MiB) each: under the 64 MiB cap alone, over it together.
(component
  (core module $libc
    (memory (export "memory") 600)
    (memory 600)
    (data (i32.const 64) "init")
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0)))
  (core instance $libc (instantiate $libc))
  (core instance $host)
  (core module $m
    (import "libc" "memory" (memory 1))
    (func (export "init") (result i32)
      (i32.store8 (i32.const 0) (i32.const 0))
      (i32.const 0)))
  (core instance $i (instantiate $m (with "libc" (instance $libc)) (with "host" (instance $host))))
  (type $code (enum "capability-denied" "declined" "invalid-argument" "not-found" "limit-exceeded" "timeout" "failed"))
  (type $perr (record (field "code" $code) (field "message" string)))
  (type $res (result (error $perr)))
  (func $init (result $res)
    (canon lift (core func $i "init") (memory (core memory $libc "memory")) (realloc (core func $libc "cabi_realloc"))))
  (instance $lc
    (export "error-code" (type $code))
    (export "plugin-error" (type $perr))
    (export "init" (func $init)))
  (export "sqler:plugin/lifecycle@0.1.0" (instance $lc))
)
