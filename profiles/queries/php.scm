; Symbols for `search_symbol` (#37, PHP). Convention:
; `@name` is the symbol name, `@definition.<kind>` the item.

(namespace_definition
  name: (namespace_name) @name) @definition.module

(class_declaration
  name: (name) @name) @definition.class

(interface_declaration
  name: (name) @name) @definition.interface

(trait_declaration
  name: (name) @name) @definition.trait

(enum_declaration
  name: (name) @name) @definition.enum

(function_definition
  name: (name) @name) @definition.function

(method_declaration
  name: (name) @name) @definition.method

(property_declaration
  (property_element
    (variable_name (name) @name))) @definition.variable

(const_declaration
  (const_element (name) @name)) @definition.constant
