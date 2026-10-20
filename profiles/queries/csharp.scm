; Symbols for `search_symbol` (#7, #70). Convention: `@name` is the symbol
; name, `@definition.<kind>` the item. Members are contained by the type
; or namespace block around them.

(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method
(local_function_statement name: (identifier) @name) @definition.function

(class_declaration name: (identifier) @name) @definition.class
(record_declaration name: (identifier) @name) @definition.class
(struct_declaration name: (identifier) @name) @definition.struct
(interface_declaration name: (identifier) @name) @definition.interface
(enum_declaration name: (identifier) @name) @definition.enum
(delegate_declaration name: (identifier) @name) @definition.type
(namespace_declaration name: (_) @name) @definition.module
(file_scoped_namespace_declaration name: (_) @name) @definition.module

(property_declaration name: (identifier) @name) @definition.variable
(field_declaration
  (variable_declaration (variable_declarator name: (identifier) @name))) @definition.variable
