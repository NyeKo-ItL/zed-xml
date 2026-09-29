; Elements behave like functions and classes for Vim/Helix text objects:
; `af`/`ac` select the whole element, `if`/`ic` its content.
(element
  (STag)
  (content)? @function.inside @class.inside
  (ETag)) @function.around @class.around

(element
  (EmptyElemTag)) @function.around @class.around

(Comment) @comment.around
