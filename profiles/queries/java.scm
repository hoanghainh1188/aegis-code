; Symbols for `search_symbol` (#7, #68). Convention: `@name` is the symbol
; name, `@definition.<kind>` the item. Methods, constructors and fields are
; contained by the class, interface, enum or record around them.

(method_declaration name: (identifier) @name) @definition.method
(constructor_declaration name: (identifier) @name) @definition.method

(class_declaration name: (identifier) @name) @definition.class
(record_declaration name: (identifier) @name) @definition.class
(enum_declaration name: (identifier) @name) @definition.enum
(interface_declaration name: (identifier) @name) @definition.interface
(annotation_type_declaration name: (identifier) @name) @definition.interface

(field_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.variable
(constant_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.constant
