; Symbols for `search_symbol` (#7, #64). Convention: `@name` is the symbol
; name, `@definition.<kind>` the whole item. Patterns are tried in order and
; the first one that captures a node wins, so methods come before functions.

(impl_item
  body: (declaration_list
    (function_item name: (identifier) @name) @definition.method))

(trait_item
  body: (declaration_list
    (function_item name: (identifier) @name) @definition.method))

(trait_item
  body: (declaration_list
    (function_signature_item name: (identifier) @name) @definition.method))

(function_item name: (identifier) @name) @definition.function
(function_signature_item name: (identifier) @name) @definition.function

(struct_item name: (type_identifier) @name) @definition.struct
(enum_item name: (type_identifier) @name) @definition.enum
(union_item name: (type_identifier) @name) @definition.union
(type_item name: (type_identifier) @name) @definition.type
(trait_item name: (type_identifier) @name) @definition.trait
(impl_item type: (_) @name) @definition.impl
(mod_item name: (identifier) @name) @definition.module
(macro_definition name: (identifier) @name) @definition.macro
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.static
