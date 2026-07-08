;; Example external plugin, compiled to WebAssembly.
;;
;; ABI (see crates/plugin/src/wasm.rs):
;;   - export "memory"
;;   - on_command(ctx_ptr: i32, ctx_len: i32) -> i64    ; (out_ptr<<32 | out_len) → JSON actions
;;   - render_panel(ctx_ptr: i32, ctx_len: i32) -> i64  ; (out_ptr<<32 | out_len) → JSON string[]
;; The returned pointers reference UTF-8 bytes in this module's linear memory.
;;
;; The guest ignores its input here. The host applies the "insert" action only because the
;; manifest was granted `edit`, and sets the panel only because it was granted `ui` — the
;; deny-by-default capability model holds across the sandbox boundary.
(module
  (memory (export "memory") 1)
  ;; command output at offset 16 (51 bytes)
  (data (i32.const 16) "[{\"action\":\"insert\",\"text\":\"// hello from wasm\\n\"}]")
  ;; panel output at offset 128 (32 bytes)
  (data (i32.const 128) "[\"wasm panel\",\"hello from wasm\"]")
  (func (export "on_command") (param i32 i32) (result i64)
    (i64.or (i64.shl (i64.const 16) (i64.const 32)) (i64.const 51)))
  (func (export "render_panel") (param i32 i32) (result i64)
    (i64.or (i64.shl (i64.const 128) (i64.const 32)) (i64.const 32))))
