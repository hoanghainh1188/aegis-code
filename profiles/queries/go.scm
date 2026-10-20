; Symbols for `search_symbol` (#7, #69). Convention: `@name` is the symbol
; name, `@definition.<kind>` the item, `@container` (optional) the text the
; container name is taken from when the item is not nested inside it (Go
; methods name their receiver type instead).

(method_declaration
  receiver: (parameter_list) @container
  name: (field_identifier) @name) @definition.method

(function_declaration name: (identifier) @name) @definition.function

(type_spec name: (type_identifier) @name type: (struct_type)) @definition.struct
(type_spec name: (type_identifier) @name type: (interface_type)) @definition.interface
(type_spec name: (type_identifier) @name) @definition.type
(type_alias name: (type_identifier) @name) @definition.type

(interface_type (method_elem name: (field_identifier) @name) @definition.method)

(const_spec name: (identifier) @name) @definition.constant
(var_spec name: (identifier) @name) @definition.variable
