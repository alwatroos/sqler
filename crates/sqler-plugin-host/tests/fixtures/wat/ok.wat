;; SPDX-License-Identifier: Apache-2.0
;; SPDX-FileCopyrightText: The SQLer Authors
;; Logs "init" at info and returns ok.
(component
  (import "sqler:plugin/log@0.1.0" (instance $log
    (type $l0 (enum "trace" "debug" "info" "warn" "error"))
    (export "level" (type $level (eq $l0)))
    (export "write" (func (param "level" $level) (param "message" string)))))
  (core module $libc
    (memory (export "memory") 1)
    (data (i32.const 64) "init")
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 0)))
  (core instance $libc (instantiate $libc))
  (core func $write (canon lower (func $log "write") (memory (core memory $libc "memory"))))
  (core instance $host (export "write" (func $write)))
  (core module $m
    (import "host" "write" (func $write (param i32 i32 i32)))
    (import "libc" "memory" (memory 1))
    ;; result<_, plugin-error> is returned through memory: tag at 0, the error record from 4 on.
    (func (export "init") (result i32)
      (call $write (i32.const 2) (i32.const 64) (i32.const 4))
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
