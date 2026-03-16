; Definitions

(class_def
  name: (_) @name) @definition.class

(module_def
  name: (_) @name) @definition.module

(struct_def
  name: (_) @name) @definition.struct

(enum_def
  name: (_) @name) @definition.enum

(lib_def
  name: (_) @name) @definition.lib

(method_def
  name: (_) @name) @definition.method

(macro_def
  name: (_) @name) @definition.macro

(fun_def
  name: (_) @name) @definition.function

(const_assign
  lhs: (constant) @name) @definition.constant

(alias
  name: (_) @name) @definition.type

; References

(call
  method: (identifier) @name) @reference.call

(named_type
  name: (_) @name) @reference.type

(constant) @name @reference.constant
