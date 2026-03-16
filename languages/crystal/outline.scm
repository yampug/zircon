; Classes, modules, structs

(class_def
  "class" @context
  name: (_) @name) @item

(module_def
  "module" @context
  name: (_) @name) @item

(struct_def
  "struct" @context
  name: (_) @name) @item

; Enums

(enum_def
  "enum" @context
  name: (_) @name) @item

; Enum members (constants inside an enum body)

(enum_def
  body: (expressions
    (constant) @name @item))

; Libs

(lib_def
  "lib" @context
  name: (_) @name) @item

; Methods

(method_def
  "def" @context
  name: (_) @name) @item

(abstract_method_def
  "abstract" @context
  name: (_) @name) @item

; Macros

(macro_def
  "macro" @context
  name: (_) @name) @item

; Functions (inside lib blocks)

(fun_def
  "fun" @context
  name: (_) @name) @item

; Constants

(const_assign
  lhs: (constant) @name) @item

; Type aliases

(alias
  "alias" @context
  name: (_) @name) @item

; Annotations

(annotation_def
  "annotation" @context
  name: (_) @name) @item
