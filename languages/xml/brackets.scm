; Delimiters of a single tag.
(STag "<" @open ">" @close (#set! rainbow.exclude))
(ETag "</" @open ">" @close (#set! rainbow.exclude))
(EmptyElemTag "<" @open "/>" @close (#set! rainbow.exclude))

; XML declaration and processing instructions.
(XMLDecl "<?" @open "?>" @close (#set! rainbow.exclude))
(PI "<?" @open "?>" @close (#set! rainbow.exclude))
(StyleSheetPI "<?" @open "?>" @close (#set! rainbow.exclude))
(XmlModelPI "<?" @open "?>" @close (#set! rainbow.exclude))

; Quoted attribute values.
(AttValue "\"" @open "\"" @close (#set! rainbow.exclude))
(AttValue "'" @open "'" @close (#set! rainbow.exclude))

; DTD brackets.
("[" @open "]" @close)
("(" @open ")" @close)

; Start tag / end tag pairs: jump and highlight between matching tags.
((element
  (STag) @open
  (ETag) @close)
  (#set! newline.only)
  (#set! rainbow.exclude))
