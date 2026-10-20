; Symbols for `search_symbol` (#7, #65). Convention: `@name` is the symbol
; name, `@definition.<kind>` the item. Patterns are tried in order and the
; first one that captures a node wins, so methods come before functions.
; A decorated definition is reported from its first decorator (the code
; widens the range to the enclosing `decorated_definition`).

(class_definition
  body: (block
    (function_definition name: (identifier) @name) @definition.method))

(class_definition
  body: (block
    (decorated_definition
      definition: (function_definition name: (identifier) @name) @definition.method)))

(function_definition name: (identifier) @name) @definition.function
(class_definition name: (identifier) @name) @definition.class

(module
  (expression_statement
    (assignment left: (identifier) @name) @definition.constant))
