; CSS inside <style> (SVG, XHTML), with or without a namespace prefix.
((element
  (STag
    (Name) @_name)
  (content
    (CharData) @injection.content))
  (#match? @_name "^([A-Za-z_][A-Za-z0-9_.-]*:)?style$")
  (#set! injection.language "css"))

((element
  (STag
    (Name) @_name)
  (content
    (CDSect
      (CData) @injection.content)))
  (#match? @_name "^([A-Za-z_][A-Za-z0-9_.-]*:)?style$")
  (#set! injection.language "css"))

; JavaScript inside <script> (SVG, XHTML), with or without a namespace prefix.
((element
  (STag
    (Name) @_name)
  (content
    (CharData) @injection.content))
  (#match? @_name "^([A-Za-z_][A-Za-z0-9_.-]*:)?script$")
  (#set! injection.language "javascript"))

((element
  (STag
    (Name) @_name)
  (content
    (CDSect
      (CData) @injection.content)))
  (#match? @_name "^([A-Za-z_][A-Za-z0-9_.-]*:)?script$")
  (#set! injection.language "javascript"))
