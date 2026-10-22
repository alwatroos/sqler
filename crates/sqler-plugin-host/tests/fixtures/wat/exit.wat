;; SPDX-License-Identifier: Apache-2.0
;; SPDX-FileCopyrightText: The SQLer Authors
;; Calls wasi:cli/exit with an error status from init.
(component
  (import "wasi:cli/exit@0.2.0" (instance $exit
    (export "exit" (func (param "status" (result))))))
  (core module $libc
    (memory (export "memory") 1)
    (data (i32.const 64) "init")
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0)))
  (core instance $libc (instantiate $libc))
  (core func $exit (canon lower (func $exit "exit")))
  (core instance $host (export "exit" (func $exit)))
  (core module $m
    (import "host" "exit" (func $exit (param i32)))
    (import "libc" "memory" (memory 1))
    (func (export "init") (result i32)
      (call $exit (i32.const 1))
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
