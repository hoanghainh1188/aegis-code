; Symbols for `search_symbol` (#7, #67), TypeScript and TSX grammars.
; Convention: `@name` is the symbol name, `@definition.<kind>` the item.
; Patterns are tried in order; the first one that captures a node wins.
; An exported item starts at its `export` (code widens to `export_statement`).

(method_definition name: (_) @name) @definition.method
(method_signature name: (_) @name) @definition.method
(abstract_method_signature name: (_) @name) @definition.method

(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

(class_declaration name: (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration name: (type_identifier) @name) @definition.interface
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.enum
(internal_module name: (_) @name) @definition.module

(program
  (lexical_declaration (variable_declarator name: (identifier) @name) @definition.variable))
(program
  (export_statement
    (lexical_declaration (variable_declarator name: (identifier) @name) @definition.variable)))
