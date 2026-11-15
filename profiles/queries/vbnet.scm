; Symbols for `search_symbol` (#37, VB.NET in the .NET profile). Convention:
; `@name` is the symbol name, `@definition.<kind>` the item.

(namespace_block
  name: (_) @name) @definition.module

(module_block
  name: (_) @name) @definition.module

(class_block
  name: (_) @name) @definition.class

(structure_block
  name: (_) @name) @definition.struct

(interface_block
  name: (_) @name) @definition.interface

(enum_block
  name: (_) @name) @definition.enum

(delegate_declaration
  name: (_) @name) @definition.type

(method_declaration
  name: (_) @name) @definition.method

(property_declaration
  name: (_) @name) @definition.variable

(const_declaration
  name: (_) @name) @definition.constant

(event_declaration
  name: (_) @name) @definition.variable
