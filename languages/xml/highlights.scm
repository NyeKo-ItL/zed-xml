"xml" @keyword
[ "version" "encoding" "standalone" ] @property
(EncName) @string.special
(VersionNum) @number
[ "yes" "no" ] @boolean

(PI) @embedded
(PI (PITarget) @keyword)
(XmlModelPI "xml-model" @keyword)
(StyleSheetPI "xml-stylesheet" @keyword)
(PseudoAtt (Name) @property)
(PseudoAtt (PseudoAttValue) @string)

(STag (Name) @tag)
(ETag (Name) @tag)
(EmptyElemTag (Name) @tag)
(Attribute (Name) @property)
(Attribute (AttValue) @string)

(EntityRef) @constant
(CharRef) @constant
(PEReference) @constant
(SystemLiteral (URI) @markup.link)

[ "<?" "?>" "<!" "]]>" "<" ">" "</" "/>" ] @punctuation.delimiter
[ "(" ")" "[" "]" ] @punctuation.bracket
[ "\"" "'" ] @punctuation.delimiter
[ "," "|" "=" ] @operator
(CharData) @markup
(CDSect
  (CDStart) @markup.heading
  (CData) @markup.raw
  "]]>" @markup.heading)
(Comment) @comment
(ERROR) @error
