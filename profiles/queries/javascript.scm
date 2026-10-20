; Symbols for `search_symbol` (#7, #67), JavaScript and JSX grammar.
; Same convention as typescript.scm, without the TypeScript-only items.

(method_definition name: (_) @name) @definition.method

(function_declaration name: (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

(class_declaration name: (identifier) @name) @definition.class

(program
  (lexical_declaration (variable_declarator name: (identifier) @name) @definition.variable))
(program
  (export_statement
    (lexical_declaration (variable_declarator name: (identifier) @name) @definition.variable)))
