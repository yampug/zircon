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

(abstract_method_def
  name: (_) @name) @definition.method

(macro_def
  name: (_) @name) @definition.macro

(fun_def
  name: (_) @name) @definition.function

(const_assign
  lhs: (constant) @name) @definition.constant

(alias
  name: (_) @name) @definition.type

; Instance variable definitions (assignments)

(assign
  lhs: (instance_var) @name) @definition.field

(op_assign
  lhs: (instance_var) @name) @definition.field

; Class variable definitions (assignments)

(assign
  lhs: (class_var) @name) @definition.field

(op_assign
  lhs: (class_var) @name) @definition.field

; Property/getter/setter macro definitions — typed form: `property name : String`

(call
  method: (identifier) @_macro_name
  arguments: (argument_list
    (type_declaration
      var: (identifier) @name))
  (#match? @_macro_name "^(property|getter|setter|class_property|class_getter|class_setter)$")) @definition.method

; Property/getter/setter macro definitions — untyped form: `getter name`

(call
  method: (identifier) @_macro_name
  arguments: (argument_list
    . (identifier) @name)
  (#match? @_macro_name "^(property|getter|setter|class_property|class_getter|class_setter)$")) @definition.method

; Property/getter/setter macro definitions — symbol form: `getter :name`

(call
  method: (identifier) @_macro_name
  arguments: (argument_list
    . (symbol) @name)
  (#match? @_macro_name "^(property|getter|setter|class_property|class_getter|class_setter)$")) @definition.method

; References

(call
  method: (identifier) @name) @reference.call

(named_type
  name: (_) @name) @reference.type

(constant) @name @reference.constant

; Instance and class variable references

(instance_var) @name @reference.field

(class_var) @name @reference.field

; Require paths

(require
  (string
    (literal_content) @name)) @reference.call
