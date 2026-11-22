; Symbols for `search_symbol` (#7, #586). Convention: `@name` is the symbol
; name, `@definition.<kind>` the item. The first pattern to capture a node
; wins, so members come before top-level declarations and interfaces and
; enums before classes. Functions, properties and nested types are
; contained by the class, interface, object or companion around them.

; Members of a class, object, companion or enum body, and top-level
; functions; not the local functions of a body.
(class_body (function_declaration (simple_identifier) @name) @definition.method)
(enum_class_body (function_declaration (simple_identifier) @name) @definition.method)
(source_file (function_declaration (simple_identifier) @name) @definition.function)

(class_declaration "interface" (type_identifier) @name) @definition.interface
(class_declaration (type_identifier) @name (enum_class_body)) @definition.enum
(class_declaration (type_identifier) @name) @definition.class
(object_declaration (type_identifier) @name) @definition.class
; A named companion; `companion object { }` has no name of its own.
(companion_object (type_identifier) @name) @definition.class

(type_alias (type_identifier) @name) @definition.type

; Properties of a type or the file, not the local `val`s of a body.
(source_file
  (property_declaration
    (modifiers (property_modifier) @_const)
    (variable_declaration (simple_identifier) @name)
    (#eq? @_const "const")) @definition.constant)
(class_body
  (property_declaration
    (modifiers (property_modifier) @_const)
    (variable_declaration (simple_identifier) @name)
    (#eq? @_const "const")) @definition.constant)
(source_file (property_declaration (variable_declaration (simple_identifier) @name)) @definition.variable)
(class_body (property_declaration (variable_declaration (simple_identifier) @name)) @definition.variable)
(enum_class_body (property_declaration (variable_declaration (simple_identifier) @name)) @definition.variable)
